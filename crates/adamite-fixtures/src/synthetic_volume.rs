use crate::{
    VolumeImage,
    builders::{AttrBuilder, BootSectorBuilder, FileNameBuilder, RecordBuilder},
};
use std::io::{self, Seek, SeekFrom, Write};

/// Workload dimensions; occupancy is the exact ratio of in-use records to slots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntheticConfig {
    /// Total 1 KiB MFT slots, including reserved and unused slots.
    pub slot_count: u32,
    /// Exact number of in-use records, including MFT and root records.
    pub in_use_records: u32,
    /// Directory percentage of ordinary records.
    pub directory_percent: u8,
    /// Primary filename length in UTF-16 units (12 through 128).
    pub name_units: u8,
    /// DOS-alias percentage of eligible files (sampling uses the ordinary record ordinal).
    pub dos_alias_percent: u8,
    /// Non-resident DATA percentage (sampling uses the ordinary record ordinal).
    pub non_resident_percent: u8,
}

impl SyntheticConfig {
    /// Creates the D2 reference mix with 25% additional free slots and reserved system slots.
    ///
    /// # Errors
    ///
    /// Rejects counts smaller than three or a slot count exceeding u32.
    pub fn reference(in_use_records: u32) -> io::Result<Self> {
        if in_use_records < 3 {
            return Err(io::Error::other("at least three in-use records required"));
        }
        let ordinary = u64::from(in_use_records - 2);
        let slot_count = u32::try_from((32 + ordinary + ordinary.div_ceil(4)).next_multiple_of(4))
            .map_err(io::Error::other)?;
        Ok(Self {
            slot_count,
            in_use_records,
            directory_percent: 5,
            name_units: 20,
            dos_alias_percent: 25,
            non_resident_percent: 30,
        })
    }

    /// Returns a versioned filename containing every configuration value for exact cache reuse.
    #[must_use]
    pub fn image_name(&self) -> String {
        format!(
            "synthetic-v1-s{}-u{}-d{}-n{}-a{}-r{}.img",
            self.slot_count,
            self.in_use_records,
            self.directory_percent,
            self.name_units,
            self.dos_alias_percent,
            self.non_resident_percent
        )
    }
}

/// Streams a deterministic MFT, bitmap and sparse payload area with bounded working memory.
#[derive(Debug)]
pub struct SyntheticVolume {
    config: SyntheticConfig,
    layout: VolumeImage,
    bitmap_lcn: u64,
    payload_lcn: u64,
    length: u64,
}

impl SyntheticVolume {
    /// Validates a workload and calculates its sparse volume layout.
    ///
    /// # Errors
    ///
    /// Rejects invalid ratios, name lengths or counts. At least 32 system slots are reserved.
    pub fn new(config: SyntheticConfig) -> io::Result<Self> {
        if config.in_use_records < 3
            || u64::from(config.slot_count) < u64::from(config.in_use_records) + 30
            || config.directory_percent > 100
            || config.dos_alias_percent > 100
            || config.non_resident_percent > 100
            || !(12..=128).contains(&config.name_units)
        {
            return Err(io::Error::other("invalid synthetic workload dimensions"));
        }
        let bitmap_lcn = 256 + (u64::from(config.slot_count) * 1024).div_ceil(4096) + 16;
        let payload_lcn = bitmap_lcn + u64::from(config.slot_count).div_ceil(8).div_ceil(4096) + 16;
        let length = (payload_lcn + u64::from(config.in_use_records) * 4 + 1024) * 4096;
        let boot = BootSectorBuilder {
            sectors: length / 512,
            mft: 256,
            mirror: 16,
            ..BootSectorBuilder::default()
        };
        let layout = VolumeImage::new(boot, length)?;
        Ok(Self {
            config,
            layout,
            bitmap_lcn,
            payload_lcn,
            length,
        })
    }

    /// Logical sparse image length, including unread file payload holes.
    #[must_use]
    pub fn len_bytes(&self) -> u64 {
        self.length
    }

    /// MFT DATA length including free slots.
    #[must_use]
    pub fn mft_bytes(&self) -> u64 {
        u64::from(self.config.slot_count) * 1024
    }

    fn ordinal(&self, index: u64) -> u64 {
        let ordinary = u64::from(self.config.in_use_records - 2);
        let free = u64::from(self.config.slot_count) - 32 - ordinary;
        let reference_slots = (32 + ordinary + ordinary.div_ceil(4)).next_multiple_of(4);
        let holes = if u64::from(self.config.slot_count) == reference_slots {
            index / 4
        } else {
            index * free / ordinary
        };
        32 + index + holes
    }

    fn filename(&self, name: Vec<u16>, parent: u64, namespace: u8, id: u16) -> Vec<u8> {
        let mut attr = AttrBuilder::resident(
            0x30,
            FileNameBuilder {
                name,
                parent: (7u64 << 48) | parent,
                namespace,
                ..FileNameBuilder::default()
            }
            .build(),
        );
        attr.id = id;
        attr.build()
    }

    fn standard(&self, directory: bool) -> Vec<u8> {
        let mut bytes = vec![0; 72];
        for time in bytes[..32].as_chunks_mut::<8>().0 {
            time.copy_from_slice(&133_000_000_000_000_000u64.to_le_bytes());
        }
        bytes[32..36].copy_from_slice(&(if directory { 16u32 } else { 32u32 }).to_le_bytes());
        AttrBuilder::resident(0x10, bytes).build()
    }

    fn record(&self, index: u64) -> io::Result<Vec<u8>> {
        let ordinal = self.ordinal(index);
        let directory = index % 100 < u64::from(self.config.directory_percent);
        let parent = if directory || self.config.directory_percent == 0 {
            5
        } else {
            self.ordinal(index / 100 * 100 + index % u64::from(self.config.directory_percent))
        };
        let suffix = if directory { ".dir" } else { ".txt" };
        let text = format!(
            "{index:0width$x}{suffix}",
            width = usize::from(self.config.name_units) - suffix.len()
        );
        let mut attributes = vec![
            self.standard(directory),
            self.filename(text.encode_utf16().collect(), parent, 1, 1),
        ];
        if !directory {
            if (index % 4 * 25 + index / 4 % 25) < u64::from(self.config.dos_alias_percent) {
                attributes.push(self.filename(
                    format!("F{index:07}.TXT").encode_utf16().collect(),
                    parent,
                    2,
                    2,
                ));
            }
            if (index % 10 * 10 + index / 10 % 10) < u64::from(self.config.non_resident_percent) {
                attributes.push(
                    self.layout
                        .non_resident(0x80, self.payload_lcn + index * 4, 4, 12_345)?
                        .build(),
                );
            } else {
                attributes.push(AttrBuilder::resident(0x80, vec![0x61; 64]).build());
            }
        }
        Ok(RecordBuilder {
            record_number: ordinal as u32,
            flags: if directory { 3 } else { 1 },
            attributes,
            ..RecordBuilder::default()
        }
        .build())
    }

    /// Writes into an empty `Write + Seek`, retaining only a record and one bitmap block.
    ///
    /// # Errors
    ///
    /// Forwards image layout, seek and write failures. Unwritten file payloads remain holes.
    pub fn write<W: Write + Seek>(&self, writer: &mut W) -> io::Result<()> {
        self.layout.write(writer)?;
        let bitmap_len = u64::from(self.config.slot_count).div_ceil(8);
        let mft = RecordBuilder {
            record_number: 0,
            attributes: vec![
                self.standard(false),
                self.filename("$MFT".encode_utf16().collect(), 5, 1, 1),
                self.layout
                    .non_resident(0x80, 256, self.mft_bytes().div_ceil(4096), self.mft_bytes())?
                    .build(),
                self.layout
                    .non_resident(0xb0, self.bitmap_lcn, bitmap_len.div_ceil(4096), bitmap_len)?
                    .build(),
            ],
            ..RecordBuilder::default()
        };
        self.layout.write_record(writer, 0, &mft.build())?;
        let root = RecordBuilder {
            record_number: 5,
            flags: 3,
            attributes: vec![self.standard(true), self.filename(vec![46], 5, 1, 1)],
            ..RecordBuilder::default()
        };
        self.layout.write_record(writer, 5, &root.build())?;
        for index in 0..u64::from(self.config.in_use_records - 2) {
            self.layout
                .write_record(writer, self.ordinal(index), &self.record(index)?)?;
        }
        writer.seek(SeekFrom::Start(self.bitmap_lcn * 4096))?;
        let mut next = 0;
        for start in (0..u64::from(self.config.slot_count)).step_by(4096 * 8) {
            let mut block = [0u8; 4096];
            let end = (start + 4096 * 8).min(u64::from(self.config.slot_count));
            if start == 0 {
                block[0] = 1 | (1 << 5);
            }
            while next < u64::from(self.config.in_use_records - 2) && self.ordinal(next) < end {
                let bit = self.ordinal(next) - start;
                block[bit as usize / 8] |= 1 << (bit % 8);
                next += 1;
            }
            writer.write_all(&block[..(end - start).div_ceil(8) as usize])?;
        }
        writer.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::{SyntheticConfig, SyntheticVolume};
    use std::io::Cursor;

    #[test]
    fn reference_dimensions_and_cache_keys_include_every_setting() -> std::io::Result<()> {
        assert_eq!(SyntheticConfig::reference(300_000)?.slot_count, 375_032);
        assert_eq!(SyntheticConfig::reference(1_000_000)?.slot_count, 1_250_032);
        let config = SyntheticConfig::reference(102)?;
        for changed in [
            SyntheticConfig {
                slot_count: config.slot_count + 1,
                ..config.clone()
            },
            SyntheticConfig {
                in_use_records: 101,
                ..config.clone()
            },
            SyntheticConfig {
                directory_percent: 6,
                ..config.clone()
            },
            SyntheticConfig {
                name_units: 21,
                ..config.clone()
            },
            SyntheticConfig {
                dos_alias_percent: 26,
                ..config.clone()
            },
            SyntheticConfig {
                non_resident_percent: 31,
                ..config.clone()
            },
        ] {
            assert_ne!(config.image_name(), changed.image_name());
        }
        Ok(())
    }

    #[test]
    fn writes_valid_records_and_an_exact_bitmap() -> std::io::Result<()> {
        let config = SyntheticConfig::reference(102)?;
        let volume = SyntheticVolume::new(config.clone())?;
        let mut writer = Cursor::new(Vec::new());
        volume.write(&mut writer)?;
        let bytes = writer.into_inner();
        assert_eq!(bytes.len() as u64, volume.len_bytes());
        let bitmap_start = volume.bitmap_lcn as usize * 4096;
        let bitmap = &bytes[bitmap_start..bitmap_start + (config.slot_count as usize).div_ceil(8)];
        assert_eq!(
            bitmap.iter().map(|byte| byte.count_ones()).sum::<u32>(),
            102
        );
        for index in 0..100 {
            let ordinal = volume.ordinal(index);
            let offset = 256 * 4096 + ordinal as usize * 1024;
            let mut raw = bytes[offset..offset + 1024].to_vec();
            let record =
                adamite_ntfs::record::FileRecord::parse(&mut raw).map_err(std::io::Error::other)?;
            assert_eq!(record.header().record_number(), Some(ordinal as u32));
        }
        assert!(
            bytes[volume.payload_lcn as usize * 4096..]
                .iter()
                .all(|byte| *byte == 0)
        );
        Ok(())
    }

    #[test]
    fn rejects_invalid_workloads() -> std::io::Result<()> {
        assert!(SyntheticConfig::reference(2).is_err());
        assert!(SyntheticConfig::reference(u32::MAX).is_err());
        let config = SyntheticConfig::reference(102)?;
        for invalid in [
            SyntheticConfig {
                slot_count: 100,
                ..config.clone()
            },
            SyntheticConfig {
                in_use_records: 0,
                ..config.clone()
            },
            SyntheticConfig {
                directory_percent: 101,
                ..config.clone()
            },
            SyntheticConfig {
                name_units: 255,
                ..config.clone()
            },
            SyntheticConfig {
                dos_alias_percent: 101,
                ..config.clone()
            },
            SyntheticConfig {
                non_resident_percent: 101,
                ..config.clone()
            },
        ] {
            assert!(SyntheticVolume::new(invalid).is_err());
        }
        Ok(())
    }
}
