//! Checked MFT cluster mappings and bounded record assembly.

use crate::error::ScanError;
use adamite_ntfs::boot::VolumeGeometry;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OwnedRun {
    pub vcn: u64,
    pub lcn: u64,
    pub clusters: u64,
}

#[derive(Debug)]
pub(crate) struct MftMapping {
    pub geometry: VolumeGeometry,
    pub runs: Vec<OwnedRun>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ReadPiece {
    pub offset: u64,
    pub len: usize,
}

impl MftMapping {
    pub(crate) fn new(geometry: VolumeGeometry, runs: Vec<OwnedRun>) -> Result<Self, ScanError> {
        let cluster = u64::from(geometry.cluster_size());
        let mut previous_end = 0;
        for (index, run) in runs.iter().enumerate() {
            let virtual_end = run
                .vcn
                .checked_add(run.clusters)
                .ok_or_else(|| ScanError::overflow("virtual run end"))?;
            virtual_end
                .checked_mul(cluster)
                .ok_or_else(|| ScanError::overflow("virtual run bytes"))?;
            let physical_end = run
                .lcn
                .checked_add(run.clusters)
                .ok_or_else(|| ScanError::overflow("physical run end"))?;
            if run.clusters == 0 || physical_end > geometry.total_clusters() {
                return Err(ScanError::OutOfVolume {
                    lcn: run.lcn,
                    clusters: run.clusters,
                });
            }
            if index != 0 && run.vcn < previous_end {
                return Err(ScanError::Bootstrap {
                    reason: "overlapping virtual MFT runs",
                });
            }
            // Inverse mapping must identify one virtual location for each physical byte.
            for other in runs.iter().take(index) {
                let other_end = other
                    .lcn
                    .checked_add(other.clusters)
                    .ok_or_else(|| ScanError::overflow("physical run end"))?;
                if run.lcn < other_end && other.lcn < physical_end {
                    return Err(ScanError::Bootstrap {
                        reason: "overlapping physical MFT runs",
                    });
                }
            }
            previous_end = virtual_end;
        }
        Ok(Self { geometry, runs })
    }

    pub(crate) fn with_runs(&self, additional: &[OwnedRun]) -> Result<Self, ScanError> {
        let mut runs = Vec::new();
        let count = self
            .runs
            .len()
            .checked_add(additional.len())
            .ok_or_else(|| ScanError::overflow("mapping run count"))?;
        runs.try_reserve_exact(count)?;
        runs.extend_from_slice(&self.runs);
        runs.extend_from_slice(additional);
        runs.sort_unstable_by_key(|run| run.vcn);
        Self::new(self.geometry, runs)
    }

    pub(crate) fn covers(&self, start: u64, len: u64) -> Result<bool, ScanError> {
        let end = start
            .checked_add(len)
            .ok_or_else(|| ScanError::overflow("mapped range end"))?;
        let mut cursor = start;
        while cursor < end {
            let remaining = end
                .checked_sub(cursor)
                .ok_or_else(|| ScanError::overflow("mapped range remainder"))?;
            let chunk = usize::try_from(remaining.min(16_777_216))
                .map_err(|_| ScanError::overflow("mapped range length"))?;
            match self.read_piece(cursor, chunk) {
                Ok(piece) => {
                    cursor = cursor
                        .checked_add(
                            u64::try_from(piece.len)
                                .map_err(|_| ScanError::overflow("mapped piece length"))?,
                        )
                        .ok_or_else(|| ScanError::overflow("mapped range cursor"))?
                }
                Err(ScanError::UnmappedRange { .. }) => return Ok(false),
                Err(error) => return Err(error),
            }
        }
        Ok(true)
    }

    /// The boot sector locates a contiguous record before its own runlist is available.
    pub(crate) fn record0(geometry: VolumeGeometry) -> Result<Self, ScanError> {
        let cluster = u64::from(geometry.cluster_size());
        let clusters = u64::from(geometry.file_record_size()).div_ceil(cluster);
        let mut runs = Vec::new();
        runs.try_reserve_exact(1)?;
        runs.push(OwnedRun {
            vcn: 0,
            lcn: geometry.mft_lcn(),
            clusters,
        });
        Self::new(geometry, runs)
    }

    /// Maps a prefix, stopping at the current extent or the supplied chunk bound.
    pub(crate) fn read_piece(
        &self,
        virtual_offset: u64,
        max_len: usize,
    ) -> Result<ReadPiece, ScanError> {
        let length = u64::try_from(max_len).map_err(|_| ScanError::overflow("request length"))?;
        virtual_offset
            .checked_add(length)
            .ok_or_else(|| ScanError::overflow("virtual request end"))?;
        if max_len == 0 {
            return Err(ScanError::InvalidReadRange {
                offset: virtual_offset,
                len: max_len,
            });
        }
        let cluster = u64::from(self.geometry.cluster_size());
        for run in &self.runs {
            let start = run
                .vcn
                .checked_mul(cluster)
                .ok_or_else(|| ScanError::overflow("virtual run start"))?;
            let end = run
                .vcn
                .checked_add(run.clusters)
                .and_then(|end| end.checked_mul(cluster))
                .ok_or_else(|| ScanError::overflow("virtual run end"))?;
            if (start..end).contains(&virtual_offset) {
                let within = virtual_offset
                    .checked_sub(start)
                    .ok_or_else(|| ScanError::overflow("run displacement"))?;
                let offset = run
                    .lcn
                    .checked_mul(cluster)
                    .and_then(|start| start.checked_add(within))
                    .ok_or_else(|| ScanError::overflow("physical request start"))?;
                let available = end
                    .checked_sub(virtual_offset)
                    .ok_or_else(|| ScanError::overflow("remaining run bytes"))?;
                let len = usize::try_from(length.min(available))
                    .map_err(|_| ScanError::overflow("request length"))?;
                return Ok(ReadPiece { offset, len });
            }
        }
        Err(ScanError::UnmappedRange {
            offset: virtual_offset,
        })
    }

    pub(crate) fn virtual_offset(&self, physical_offset: u64) -> Result<u64, ScanError> {
        let cluster = u64::from(self.geometry.cluster_size());
        for run in &self.runs {
            let start = run
                .lcn
                .checked_mul(cluster)
                .ok_or_else(|| ScanError::overflow("physical run start"))?;
            let end = run
                .lcn
                .checked_add(run.clusters)
                .and_then(|end| end.checked_mul(cluster))
                .ok_or_else(|| ScanError::overflow("physical run end"))?;
            if (start..end).contains(&physical_offset) {
                let within = physical_offset
                    .checked_sub(start)
                    .ok_or_else(|| ScanError::overflow("run displacement"))?;
                return run
                    .vcn
                    .checked_mul(cluster)
                    .and_then(|start| start.checked_add(within))
                    .ok_or_else(|| ScanError::overflow("virtual byte offset"));
            }
        }
        Err(ScanError::UnmappedRange {
            offset: physical_offset,
        })
    }
}

/// One raw record, assembled in virtual order from chunk/extent-bounded responses.
#[derive(Debug)]
pub(crate) struct RecordAssembly {
    start: u64,
    len: usize,
    planned: usize,
    bytes: Vec<u8>,
}

impl RecordAssembly {
    pub(crate) fn new(start: u64, len: usize) -> Result<Self, ScanError> {
        start
            .checked_add(u64::try_from(len).map_err(|_| ScanError::overflow("record length"))?)
            .ok_or_else(|| ScanError::overflow("record end"))?;
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(len)?;
        Ok(Self {
            start,
            len,
            planned: 0,
            bytes,
        })
    }

    pub(crate) fn next_piece(
        &self,
        mapping: &MftMapping,
        chunk: usize,
    ) -> Result<Option<ReadPiece>, ScanError> {
        let remaining = self
            .len
            .checked_sub(self.planned)
            .ok_or_else(|| ScanError::overflow("unplanned record bytes"))?;
        if remaining == 0 {
            return Ok(None);
        }
        let offset = self
            .start
            .checked_add(
                u64::try_from(self.planned).map_err(|_| ScanError::overflow("record cursor"))?,
            )
            .ok_or_else(|| ScanError::overflow("record cursor"))?;
        mapping.read_piece(offset, remaining.min(chunk)).map(Some)
    }

    pub(crate) fn planned(&mut self, len: usize) -> Result<(), ScanError> {
        let end = self
            .planned
            .checked_add(len)
            .ok_or_else(|| ScanError::overflow("planned record bytes"))?;
        if end > self.len {
            return Err(ScanError::Bootstrap {
                reason: "record request exceeds assembly",
            });
        }
        self.planned = end;
        Ok(())
    }

    pub(crate) fn append(&mut self, bytes: &[u8]) -> Result<(), ScanError> {
        let end = self
            .bytes
            .len()
            .checked_add(bytes.len())
            .ok_or_else(|| ScanError::overflow("assembled record bytes"))?;
        if end > self.planned {
            return Err(ScanError::Bootstrap {
                reason: "response exceeds planned record bytes",
            });
        }
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }

    pub(crate) fn complete(&self) -> bool {
        self.bytes.len() == self.len
    }
    pub(crate) fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::{MftMapping, OwnedRun, ReadPiece, RecordAssembly, ScanError};
    use adamite_fixtures::builders::BootSectorBuilder;
    use adamite_ntfs::boot::VolumeGeometry;
    use std::error::Error;

    fn geometry() -> Result<VolumeGeometry, ScanError> {
        VolumeGeometry::from_boot_sector(
            &BootSectorBuilder {
                cluster_encoding: 1,
                sectors: 256,
                ..BootSectorBuilder::default()
            }
            .build(),
        )
        .map_err(|source| ScanError::Geometry { source })
    }

    #[test]
    fn bootstrap_record0_mapping_splits_extents_and_reassembles_record()
    -> Result<(), Box<dyn Error>> {
        let mapping = MftMapping::new(
            geometry()?,
            vec![
                OwnedRun {
                    vcn: 0,
                    lcn: 8,
                    clusters: 1,
                },
                OwnedRun {
                    vcn: 1,
                    lcn: 4,
                    clusters: 2,
                },
            ],
        )?;
        let mut assembly = RecordAssembly::new(256, 1024)?;
        let mut pieces = Vec::new();
        let mut virtual_offsets = Vec::new();
        while let Some(piece) = assembly.next_piece(&mapping, 512)? {
            let expected = assembly
                .start
                .checked_add(u64::try_from(assembly.planned)?)
                .ok_or("assembly cursor overflow")?;
            assert_eq!(mapping.virtual_offset(piece.offset)?, expected);
            virtual_offsets.push(expected);
            assembly.planned(piece.len)?;
            pieces.push(piece);
            assembly.append(&vec![42; piece.len])?;
        }
        assert_eq!(virtual_offsets, vec![256, 512, 1024]);
        assert_eq!(
            pieces,
            vec![
                ReadPiece {
                    offset: 4352,
                    len: 256
                },
                ReadPiece {
                    offset: 2048,
                    len: 512
                },
                ReadPiece {
                    offset: 2560,
                    len: 256
                },
            ]
        );
        assert!(assembly.complete());
        assert_eq!(assembly.into_bytes(), vec![42; 1024]);
        assert!(matches!(
            mapping.read_piece(1536, 1),
            Err(ScanError::UnmappedRange { .. })
        ));
        assert!(matches!(
            mapping.virtual_offset(3072),
            Err(ScanError::UnmappedRange { .. })
        ));
        Ok(())
    }

    #[test]
    fn bootstrap_record0_mapping_rejects_overflow_out_of_volume_and_overlap()
    -> Result<(), Box<dyn Error>> {
        for run in [
            OwnedRun {
                vcn: u64::MAX,
                lcn: 4,
                clusters: 1,
            },
            OwnedRun {
                vcn: u64::MAX / 512,
                lcn: 4,
                clusters: 1,
            },
            OwnedRun {
                vcn: 0,
                lcn: u64::MAX,
                clusters: 1,
            },
        ] {
            assert!(matches!(
                MftMapping::new(geometry()?, vec![run]),
                Err(ScanError::Overflow { .. })
            ));
        }
        for run in [
            OwnedRun {
                vcn: 0,
                lcn: 255,
                clusters: 2,
            },
            OwnedRun {
                vcn: 0,
                lcn: 4,
                clusters: 0,
            },
        ] {
            assert!(matches!(
                MftMapping::new(geometry()?, vec![run]),
                Err(ScanError::OutOfVolume { .. })
            ));
        }
        let first = OwnedRun {
            vcn: 0,
            lcn: 4,
            clusters: 2,
        };
        for next in [
            OwnedRun {
                vcn: 1,
                lcn: 8,
                clusters: 1,
            },
            OwnedRun {
                vcn: 2,
                lcn: 5,
                clusters: 2,
            },
        ] {
            assert!(matches!(
                MftMapping::new(geometry()?, vec![first, next]),
                Err(ScanError::Bootstrap { .. })
            ));
        }
        let mapping = MftMapping::new(geometry()?, vec![first])?;
        assert!(matches!(
            mapping.read_piece(u64::MAX, 1),
            Err(ScanError::Overflow { .. })
        ));
        assert!(matches!(
            mapping.read_piece(0, 0),
            Err(ScanError::InvalidReadRange { .. })
        ));
        assert!(matches!(
            RecordAssembly::new(u64::MAX, 1),
            Err(ScanError::Overflow { .. })
        ));
        Ok(())
    }

    #[test]
    fn bootstrap_record0_mapping_preserves_unknown_gaps() -> Result<(), Box<dyn Error>> {
        let mapping = MftMapping::new(
            geometry()?,
            vec![
                OwnedRun {
                    vcn: 0,
                    lcn: 4,
                    clusters: 1,
                },
                OwnedRun {
                    vcn: 2,
                    lcn: 8,
                    clusters: 1,
                },
            ],
        )?;
        assert_eq!(mapping.read_piece(0, 1024)?.len, 512);
        assert!(matches!(
            mapping.read_piece(512, 1),
            Err(ScanError::UnmappedRange { .. })
        ));
        assert_eq!(mapping.read_piece(1024, 512)?.offset, 4096);
        Ok(())
    }
}
