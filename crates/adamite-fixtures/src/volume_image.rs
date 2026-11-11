use crate::{
    builders::{AttrBuilder, BootSectorBuilder, NonResidentModel},
    runlist_encoder::{RunModel, RunlistEncoder},
};
use adamite_ntfs::boot::VolumeGeometry;
use std::io::{self, Seek, SeekFrom, Write};

/// Sparse NTFS image layout shared by custom scanner fixtures and synthetic workloads.
#[derive(Debug, Clone)]
pub struct VolumeImage {
    boot: BootSectorBuilder,
    length: u64,
    cluster_size: u64,
    record_size: u64,
}

impl VolumeImage {
    /// Validates boot geometry and the image length.
    ///
    /// # Errors
    ///
    /// Rejects invalid geometry or an image too short to contain record zero.
    pub fn new(boot: BootSectorBuilder, length: u64) -> io::Result<Self> {
        let geometry = VolumeGeometry::from_boot_sector(&boot.build()).map_err(io::Error::other)?;
        let result = Self {
            boot,
            length,
            cluster_size: u64::from(geometry.cluster_size()),
            record_size: u64::from(geometry.file_record_size()),
        };
        result.record_offset(0)?;
        Ok(result)
    }

    /// Initializes an empty writer with the boot sector and sparse logical length.
    ///
    /// # Errors
    ///
    /// Returns seek or write failures. The writer must be empty before initialization.
    pub fn write<W: Write + Seek>(&self, writer: &mut W) -> io::Result<()> {
        writer.seek(SeekFrom::Start(self.length - 1))?;
        writer.write_all(&[0])?;
        writer.seek(SeekFrom::Start(0))?;
        writer.write_all(&self.boot.build())
    }

    /// Writes one record into the contiguous MFT region.
    ///
    /// # Errors
    ///
    /// Rejects out-of-range records or incorrect record lengths, and forwards I/O failures.
    pub fn write_record<W: Write + Seek>(
        &self,
        writer: &mut W,
        ordinal: u64,
        bytes: &[u8],
    ) -> io::Result<()> {
        if bytes.len() as u64 != self.record_size {
            return Err(io::Error::other("incorrect record length"));
        }
        writer.seek(SeekFrom::Start(self.record_offset(ordinal)?))?;
        writer.write_all(bytes)
    }

    fn record_offset(&self, ordinal: u64) -> io::Result<u64> {
        let offset = self
            .boot
            .mft
            .checked_mul(self.cluster_size)
            .and_then(|base| {
                ordinal
                    .checked_mul(self.record_size)
                    .and_then(|relative| base.checked_add(relative))
            })
            .ok_or_else(|| io::Error::other("record offset overflow"))?;
        if offset
            .checked_add(self.record_size)
            .is_none_or(|end| end > self.length)
        {
            return Err(io::Error::other("record outside image"));
        }
        Ok(offset)
    }

    /// Builds one contiguous non-resident attribute using this image's cluster geometry.
    ///
    /// # Errors
    ///
    /// Rejects zero clusters or sizes outside the NTFS signed size range.
    pub fn non_resident(
        &self,
        kind: u32,
        lcn: u64,
        clusters: u64,
        size: u64,
    ) -> io::Result<AttrBuilder> {
        let allocated = clusters
            .checked_mul(self.cluster_size)
            .ok_or_else(|| io::Error::other("extent overflow"))?;
        let allocated = i64::try_from(allocated).map_err(io::Error::other)?;
        let size = i64::try_from(size).map_err(io::Error::other)?;
        let highest = i64::try_from(
            clusters
                .checked_sub(1)
                .ok_or_else(|| io::Error::other("empty extent"))?,
        )
        .map_err(io::Error::other)?;
        Ok(AttrBuilder::non_resident(
            kind,
            NonResidentModel {
                highest,
                sizes: [allocated, size, size, 0],
                mapping_pairs: RunlistEncoder {
                    runs: vec![RunModel {
                        lcn: Some(lcn),
                        len: clusters,
                    }],
                }
                .encode(0),
                ..NonResidentModel::default()
            },
        ))
    }
}
