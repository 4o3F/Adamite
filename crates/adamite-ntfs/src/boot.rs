//! Boot-sector geometry and checked conversion of cluster extents to byte ranges.

use core::{fmt, ops::Range};

use snafu::{OptionExt, Snafu, ensure};

use crate::util;

/// Largest cluster supported by the NTFS geometry contract, in bytes.
const MAX_CLUSTER_SIZE: u32 = 2_097_152;

/// Independent volume geometry values used to reconstruct validated geometry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GeometryFields {
    /// Bytes per logical sector: 512, 1024, 2048 or 4096.
    pub bytes_per_sector: u16,
    /// Decoded sectors per cluster, a power of two from 1 through 4096.
    pub sectors_per_cluster: u32,
    /// File-record size in bytes, a power of two from 512 through 65536.
    pub file_record_size: u32,
    /// Index-record size in bytes, a power of two from 512 through 65536.
    pub index_record_size: u32,
    /// Total sector count, including sectors in a final incomplete cluster.
    pub total_sectors: u64,
    /// Starting logical cluster number of the `$MFT`.
    pub mft_lcn: u64,
    /// Starting logical cluster number of the `$MFTMirr`.
    pub mftmirr_lcn: u64,
    /// Volume serial number, preserved without restrictions.
    pub serial: u64,
}

/// Identifies an independent geometry field checked during construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum GeometryField {
    /// Bytes per logical sector.
    BytesPerSector,
    /// Decoded sectors per cluster.
    SectorsPerCluster,
    /// File-record size in bytes.
    FileRecordSize,
    /// Index-record size in bytes.
    IndexRecordSize,
    /// Total sector count.
    TotalSectors,
    /// Starting logical cluster number of the `$MFT`.
    MftLcn,
    /// Starting logical cluster number of the `$MFTMirr`.
    MftMirrLcn,
}

impl fmt::Display for GeometryField {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::BytesPerSector => "bytes_per_sector",
            Self::SectorsPerCluster => "sectors_per_cluster",
            Self::FileRecordSize => "file_record_size",
            Self::IndexRecordSize => "index_record_size",
            Self::TotalSectors => "total_sectors",
            Self::MftLcn => "mft_lcn",
            Self::MftMirrLcn => "mftmirr_lcn",
        })
    }
}

/// Validated volume geometry constructed from boot-sector bytes or independent fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VolumeGeometry {
    bytes_per_sector: u16,
    sectors_per_cluster: u32,
    cluster_size: u32,
    file_record_size: u32,
    index_record_size: u32,
    total_sectors: u64,
    total_clusters: u64,
    total_bytes: u64,
    mft_lcn: u64,
    mftmirr_lcn: u64,
    serial: u64,
}

impl VolumeGeometry {
    /// Returns the independent values needed to reconstruct this geometry.
    #[must_use]
    pub const fn fields(self) -> GeometryFields {
        GeometryFields {
            bytes_per_sector: self.bytes_per_sector,
            sectors_per_cluster: self.sectors_per_cluster,
            file_record_size: self.file_record_size,
            index_record_size: self.index_record_size,
            total_sectors: self.total_sectors,
            mft_lcn: self.mft_lcn,
            mftmirr_lcn: self.mftmirr_lcn,
            serial: self.serial,
        }
    }

    /// Validates independent fields and recomputes cluster size, cluster count and volume bytes.
    ///
    /// # Errors
    ///
    /// Returns [`BootError::InvalidField`] with the input field's identifier and value for
    /// unsupported sizes, invalid sector counts or MFT locations without room for
    /// one file record within complete clusters. Cluster-size failures identify
    /// `sectors_per_cluster`; volume-size failures identify `total_sectors`.
    pub fn from_fields(fields: GeometryFields) -> Result<Self, BootError> {
        let GeometryFields {
            bytes_per_sector,
            sectors_per_cluster,
            file_record_size,
            index_record_size,
            total_sectors,
            mft_lcn,
            mftmirr_lcn,
            serial,
        } = fields;
        ensure!(
            valid_sector_size(bytes_per_sector),
            InvalidFieldSnafu {
                field: GeometryField::BytesPerSector,
                value: u64::from(bytes_per_sector),
            }
        );
        let cluster_size = checked_cluster_size(bytes_per_sector, sectors_per_cluster).context(
            InvalidFieldSnafu {
                field: GeometryField::SectorsPerCluster,
                value: u64::from(sectors_per_cluster),
            },
        )?;
        for (field, size) in [
            (GeometryField::FileRecordSize, file_record_size),
            (GeometryField::IndexRecordSize, index_record_size),
        ] {
            ensure!(
                valid_record_size(size),
                InvalidFieldSnafu {
                    field,
                    value: u64::from(size),
                }
            );
        }
        let total_error = || InvalidFieldSnafu {
            field: GeometryField::TotalSectors,
            value: total_sectors,
        };
        ensure!(
            (1..=i64::MAX as u64).contains(&total_sectors),
            total_error()
        );
        let total_bytes = total_sectors
            .checked_mul(u64::from(bytes_per_sector))
            .with_context(total_error)?;
        let total_clusters = total_sectors
            .checked_div(u64::from(sectors_per_cluster))
            .with_context(total_error)?;
        ensure!(total_clusters > 0, total_error());
        let complete_bytes = total_clusters
            .checked_mul(u64::from(cluster_size))
            .with_context(total_error)?;
        for (field, lcn) in [
            (GeometryField::MftLcn, mft_lcn),
            (GeometryField::MftMirrLcn, mftmirr_lcn),
        ] {
            let fits_record = lcn
                .checked_mul(u64::from(cluster_size))
                .and_then(|start| start.checked_add(u64::from(file_record_size)))
                .is_some_and(|end| end <= complete_bytes);
            ensure!(fits_record, InvalidFieldSnafu { field, value: lcn });
        }
        Ok(Self {
            bytes_per_sector,
            sectors_per_cluster,
            cluster_size,
            file_record_size,
            index_record_size,
            total_sectors,
            total_clusters,
            total_bytes,
            mft_lcn,
            mftmirr_lcn,
            serial,
        })
    }

    /// Returns bytes per sector from the u16 at boot-sector offset 0x0B.
    #[must_use]
    pub const fn bytes_per_sector(self) -> u16 {
        self.bytes_per_sector
    }
    /// Returns decoded sectors per cluster from the byte at offset 0x0D.
    #[must_use]
    pub const fn sectors_per_cluster(self) -> u32 {
        self.sectors_per_cluster
    }
    /// Returns cluster bytes derived from offsets 0x0B and 0x0D.
    #[must_use]
    pub const fn cluster_size(self) -> u32 {
        self.cluster_size
    }
    /// Returns file-record bytes decoded from the signed byte at offset 0x40.
    #[must_use]
    pub const fn file_record_size(self) -> u32 {
        self.file_record_size
    }
    /// Returns index-record bytes decoded from the signed byte at offset 0x44.
    #[must_use]
    pub const fn index_record_size(self) -> u32 {
        self.index_record_size
    }
    /// Returns the total sector count from the u64 at offset 0x28.
    #[must_use]
    pub const fn total_sectors(self) -> u64 {
        self.total_sectors
    }
    /// Returns complete clusters derived from sector count 0x28 and cluster encoding 0x0D.
    #[must_use]
    pub const fn total_clusters(self) -> u64 {
        self.total_clusters
    }
    /// Returns volume bytes derived from sector count 0x28 and sector bytes 0x0B.
    #[must_use]
    pub const fn total_bytes(self) -> u64 {
        self.total_bytes
    }
    /// Returns the `$MFT` LCN from the u64 at offset 0x30.
    #[must_use]
    pub const fn mft_lcn(self) -> u64 {
        self.mft_lcn
    }
    /// Returns the `$MFTMirr` LCN from the u64 at offset 0x38.
    #[must_use]
    pub const fn mftmirr_lcn(self) -> u64 {
        self.mftmirr_lcn
    }
    /// Returns the volume serial from the u64 at offset 0x48.
    #[must_use]
    pub const fn serial(self) -> u64 {
        self.serial
    }

    /// Converts a nonempty extent within complete volume clusters to a byte range.
    ///
    /// # Errors
    ///
    /// Returns [`BootError::InvalidClusterRange`] on overflow, zero count,
    /// or an extent extending beyond the last complete cluster.
    pub fn cluster_byte_range(self, lcn: u64, count: u64) -> Result<Range<u64>, BootError> {
        let error = || InvalidClusterRangeSnafu { lcn, count };
        ensure!(count > 0, InvalidClusterRangeSnafu { lcn, count });
        let end = lcn.checked_add(count).with_context(error)?;
        ensure!(
            end <= self.total_clusters,
            InvalidClusterRangeSnafu { lcn, count }
        );
        let size = u64::from(self.cluster_size);
        Ok(
            lcn.checked_mul(size).with_context(error)?
                ..end.checked_mul(size).with_context(error)?,
        )
    }

    /// Decodes boot-sector identification, sizes, then volume and MFT numeric relationships.
    ///
    /// Only the first 512 bytes are interpreted. Boot code and reserved bytes are ignored.
    ///
    /// # Errors
    ///
    /// Returns [`BootError`] in validation order: input extent, identification,
    /// sector/cluster/record sizes, total sectors, then MFT and MFTMirr locations.
    pub fn from_boot_sector(input: &[u8]) -> Result<Self, BootError> {
        let boot = input.get(..512).context(TruncatedSnafu)?;
        ensure!(
            boot.get(3..11) == Some(b"NTFS    ") && boot.get(510..512) == Some(&[0x55, 0xaa]),
            IdentificationSnafu
        );
        let bytes_per_sector = util::read_u16(boot, 0x0b).context(TruncatedSnafu)?;
        ensure!(
            valid_sector_size(bytes_per_sector),
            InvalidSectorSizeSnafu {
                size: bytes_per_sector
            }
        );
        let encoded = util::read_u8(boot, 0x0d).context(TruncatedSnafu)?;
        let sectors_per_cluster = match encoded {
            1..=128 if encoded.is_power_of_two() => u32::from(encoded),
            0xf4..=0xff => 1u32
                .checked_shl(
                    256u32
                        .checked_sub(u32::from(encoded))
                        .context(InvalidClusterEncodingSnafu { encoding: encoded })?,
                )
                .context(InvalidClusterEncodingSnafu { encoding: encoded })?,
            _ => return InvalidClusterEncodingSnafu { encoding: encoded }.fail(),
        };
        let cluster_size =
            checked_cluster_size(bytes_per_sector, sectors_per_cluster).ok_or_else(|| {
                match u32::from(bytes_per_sector).checked_mul(sectors_per_cluster) {
                    Some(size) => BootError::ClusterTooLarge { size },
                    None => BootError::InvalidClusterEncoding { encoding: encoded },
                }
            })?;
        let file_encoding = util::read_i8(boot, 0x40).context(TruncatedSnafu)?;
        let index_encoding = util::read_i8(boot, 0x44).context(TruncatedSnafu)?;
        let file_record_size = decode_record_size(file_encoding, cluster_size).context(
            InvalidFileRecordSizeSnafu {
                encoding: file_encoding,
            },
        )?;
        let index_record_size = decode_record_size(index_encoding, cluster_size).context(
            InvalidIndexRecordSizeSnafu {
                encoding: index_encoding,
            },
        )?;
        let total_sectors = util::read_u64(boot, 0x28).context(TruncatedSnafu)?;
        let mft_lcn = util::read_u64(boot, 0x30).context(TruncatedSnafu)?;
        let mftmirr_lcn = util::read_u64(boot, 0x38).context(TruncatedSnafu)?;
        Self::from_fields(GeometryFields {
            bytes_per_sector,
            sectors_per_cluster,
            file_record_size,
            index_record_size,
            total_sectors,
            mft_lcn,
            mftmirr_lcn,
            serial: util::read_u64(boot, 0x48).context(TruncatedSnafu)?,
        })
        .map_err(|error| match error {
            BootError::InvalidField {
                field: GeometryField::TotalSectors,
                value,
            } => BootError::InvalidTotalSectors { sectors: value },
            BootError::InvalidField {
                field: GeometryField::MftLcn,
                value,
            } => BootError::MftOutOfVolume { lcn: value },
            BootError::InvalidField {
                field: GeometryField::MftMirrLcn,
                value,
            } => BootError::MftMirrorOutOfVolume { lcn: value },
            other => other,
        })
    }
}

fn valid_sector_size(size: u16) -> bool {
    matches!(size, 512 | 1024 | 2048 | 4096)
}

fn checked_cluster_size(bytes_per_sector: u16, sectors_per_cluster: u32) -> Option<u32> {
    if !sectors_per_cluster.is_power_of_two() || !(1..=4096).contains(&sectors_per_cluster) {
        return None;
    }
    let size = u32::from(bytes_per_sector).checked_mul(sectors_per_cluster)?;
    (size <= MAX_CLUSTER_SIZE).then_some(size)
}

fn valid_record_size(size: u32) -> bool {
    size.is_power_of_two() && (512..=65536).contains(&size)
}

fn decode_record_size(encoding: i8, cluster_size: u32) -> Option<u32> {
    let size = if encoding > 0 {
        u32::try_from(encoding).ok()?.checked_mul(cluster_size)?
    } else if encoding < 0 {
        1u32.checked_shl(u32::from(encoding.unsigned_abs()))?
    } else {
        return None;
    };
    valid_record_size(size).then_some(size)
}

/// Boot-sector or cluster-range validation failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Snafu)]
#[snafu(visibility(pub(crate)))]
#[non_exhaustive]
pub enum BootError {
    /// An independent geometry field violates a size, count or location constraint.
    #[snafu(display("invalid geometry field {field}: {value}"))]
    InvalidField {
        /// Invalid input field in [`GeometryFields`].
        field: GeometryField,
        /// Supplied value of the invalid field.
        value: u64,
    },
    /// Fewer than 512 boot-sector bytes were supplied.
    #[snafu(display("boot sector is shorter than 512 bytes"))]
    Truncated,
    /// The OEM identifier or end signature differs from NTFS.
    #[snafu(display("invalid NTFS boot-sector identification"))]
    Identification,
    /// The sector size is outside the supported set.
    #[snafu(display("invalid sector size {size}"))]
    InvalidSectorSize {
        /// The encoded byte size.
        size: u16,
    },
    /// The cluster byte is neither a supported literal nor exponent.
    #[snafu(display("invalid sectors-per-cluster encoding {encoding:#04x}"))]
    InvalidClusterEncoding {
        /// The encoded byte.
        encoding: u8,
    },
    /// The decoded cluster exceeds 2 MiB.
    #[snafu(display("cluster size {size} exceeds 2 MiB"))]
    ClusterTooLarge {
        /// The decoded byte size.
        size: u32,
    },
    /// The file-record size is invalid.
    #[snafu(display("invalid file-record size encoding {encoding}"))]
    InvalidFileRecordSize {
        /// The signed size byte.
        encoding: i8,
    },
    /// The index-record size is invalid.
    #[snafu(display("invalid index-record size encoding {encoding}"))]
    InvalidIndexRecordSize {
        /// The signed size byte.
        encoding: i8,
    },
    /// Sector count is zero, too large, overflows bytes, or yields no clusters.
    #[snafu(display("invalid total sector count {sectors}"))]
    InvalidTotalSectors {
        /// The encoded sector count.
        sectors: u64,
    },
    /// The MFT location does not leave room for a file record.
    #[snafu(display("MFT LCN {lcn} is outside the volume"))]
    MftOutOfVolume {
        /// The encoded cluster address.
        lcn: u64,
    },
    /// The MFT mirror location does not leave room for a file record.
    #[snafu(display("MFT mirror LCN {lcn} is outside the volume"))]
    MftMirrorOutOfVolume {
        /// The encoded cluster address.
        lcn: u64,
    },
    /// A requested cluster extent is empty, overflows, or exceeds the volume.
    #[snafu(display("invalid cluster extent at LCN {lcn} with count {count}"))]
    InvalidClusterRange {
        /// First requested cluster.
        lcn: u64,
        /// Requested cluster count.
        count: u64,
    },
}
