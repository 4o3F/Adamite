//! Packed NTFS record references and well-known system record numbers.

use core::fmt;

use snafu::{Snafu, ensure};

/// Number of low bits reserved for a record number in an NTFS file reference.
const RECORD_BITS: u32 = 48;
/// Largest record number representable in an NTFS file reference.
const RECORD_MASK: u64 = 0x0000_ffff_ffff_ffff;

/// The `$MFT` record number.
pub const MFT_RECORD: u64 = 0;
/// The `$MFTMirr` record number.
pub const MFT_MIRROR_RECORD: u64 = 1;
/// The `$LogFile` record number.
pub const LOG_FILE_RECORD: u64 = 2;
/// The `$Volume` record number.
pub const VOLUME_RECORD: u64 = 3;
/// The `$AttrDef` record number.
pub const ATTR_DEF_RECORD: u64 = 4;
/// The root directory record number.
pub const ROOT_RECORD: u64 = 5;
/// The `$Bitmap` record number.
pub const BITMAP_RECORD: u64 = 6;
/// The `$Boot` record number.
pub const BOOT_RECORD: u64 = 7;
/// The `$BadClus` record number.
pub const BAD_CLUS_RECORD: u64 = 8;
/// The `$Secure` record number.
pub const SECURE_RECORD: u64 = 9;
/// The `$UpCase` record number.
pub const UP_CASE_RECORD: u64 = 10;
/// The `$Extend` record number.
pub const EXTEND_RECORD: u64 = 11;
/// The first record number outside the reserved system-record range.
pub const FIRST_NON_RESERVED_RECORD: u64 = 16;

/// A lossless 48-bit record number and 16-bit reuse sequence number.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileRef(u64);

impl FileRef {
    /// Preserves every bit of an on-disk reference, including a zero sequence.
    #[must_use]
    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    /// Packs a record number and its reuse sequence without truncation.
    ///
    /// # Errors
    ///
    /// Returns [`FileRefError::RecordOutOfRange`] if `record` is at least 2^48.
    pub fn new(record: u64, sequence: u16) -> Result<Self, FileRefError> {
        ensure!(record <= RECORD_MASK, RecordOutOfRangeSnafu { record });
        Ok(Self(record | (u64::from(sequence) << RECORD_BITS)))
    }

    /// Returns the complete packed reference.
    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }

    /// Returns the low 48 bits identifying the MFT record.
    #[must_use]
    pub const fn record(self) -> u64 {
        self.0 & RECORD_MASK
    }

    /// Returns the high 16 bits identifying the record's reuse sequence.
    #[must_use]
    pub const fn sequence(self) -> u16 {
        (self.0 >> RECORD_BITS) as u16
    }

    /// Reports whether both the record number and sequence number are zero.
    #[must_use]
    pub const fn is_null(self) -> bool {
        self.0 == 0
    }

    /// Decodes all eight little-endian bytes without validating the reference's target.
    #[must_use]
    pub const fn from_le_bytes(bytes: [u8; 8]) -> Self {
        Self(u64::from_le_bytes(bytes))
    }

    /// Encodes all eight bytes in little-endian order.
    #[must_use]
    pub const fn to_le_bytes(self) -> [u8; 8] {
        self.0.to_le_bytes()
    }
}

impl fmt::Debug for FileRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}:{}", self.record(), self.sequence())
    }
}

/// Failure to construct a representable file reference.
///
/// `ScanStats` mapping: [`Self::RecordOutOfRange`] counts as an invalid file
/// reference. The core owns and updates that counter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Snafu)]
#[snafu(visibility(pub(crate)))]
#[non_exhaustive]
pub enum FileRefError {
    /// The supplied record number needs more than 48 bits.
    #[snafu(display("record number {record} exceeds the 48-bit file reference range"))]
    RecordOutOfRange {
        /// The record number that could not be encoded.
        record: u64,
    },
}
