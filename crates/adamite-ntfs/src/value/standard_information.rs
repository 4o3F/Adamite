//! Eager standard-information fields and raw Windows file metadata.

use super::{InvalidStandardInformationLengthSnafu, TruncatedValueSnafu, ValueError};
use crate::{
    attribute::{Attribute, AttributeType},
    util,
};
use snafu::{OptionExt, ensure};

/// Raw Windows file-attribute bits, preserving unknown values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileAttributes(u32);

impl FileAttributes {
    /// The file is read-only.
    pub const READ_ONLY: Self = Self(0x0001);
    /// The file is hidden.
    pub const HIDDEN: Self = Self(0x0002);
    /// The file is a system file.
    pub const SYSTEM: Self = Self(0x0004);
    /// The entry describes a directory.
    pub const DIRECTORY: Self = Self(0x0010);
    /// The file has the archive bit set.
    pub const ARCHIVE: Self = Self(0x0020);
    /// The file has normal attributes.
    pub const NORMAL: Self = Self(0x0080);
    /// The file is temporary.
    pub const TEMPORARY: Self = Self(0x0100);
    /// The file has sparse data.
    pub const SPARSE_FILE: Self = Self(0x0200);
    /// The file has a reparse point.
    pub const REPARSE_POINT: Self = Self(0x0400);
    /// The file is compressed.
    pub const COMPRESSED: Self = Self(0x0800);
    /// The file's data is offline.
    pub const OFFLINE: Self = Self(0x1000);
    /// The file is excluded from content indexing.
    pub const NOT_CONTENT_INDEXED: Self = Self(0x2000);
    /// The file is encrypted.
    pub const ENCRYPTED: Self = Self(0x4000);
    /// Preserves the complete on-disk field.
    #[must_use]
    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }
    /// Returns the original attribute bits.
    #[must_use]
    pub const fn raw(self) -> u32 {
        self.0
    }
    /// Reports whether every requested attribute bit is present.
    #[must_use]
    pub const fn contains(self, flags: Self) -> bool {
        self.0 & flags.0 == flags.0
    }
}

/// Four FILETIMEs reported as raw u64 values without time-zone conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileTimes {
    created: u64,
    modified: u64,
    record_changed: u64,
    accessed: u64,
}

impl FileTimes {
    pub(super) fn parse(input: &[u8], base: usize) -> Result<Self, ValueError> {
        let read = |index| {
            util::element_offset(base, index, 8)
                .and_then(|offset| util::read_u64(input, offset))
                .context(TruncatedValueSnafu)
        };
        Ok(Self {
            created: read(0)?,
            modified: read(1)?,
            record_changed: read(2)?,
            accessed: read(3)?,
        })
    }
    /// Returns the creation FILETIME.
    #[must_use]
    pub const fn created(self) -> u64 {
        self.created
    }
    /// Returns the last data-modification FILETIME.
    #[must_use]
    pub const fn modified(self) -> u64 {
        self.modified
    }
    /// Returns the last record-change FILETIME.
    #[must_use]
    pub const fn record_changed(self) -> u64 {
        self.record_changed
    }
    /// Returns the last-access FILETIME.
    #[must_use]
    pub const fn accessed(self) -> u64 {
        self.accessed
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Extended {
    owner_id: u32,
    security_id: u32,
    quota_charged: u64,
    usn: u64,
}

/// Standard information from a legacy 48-byte or extended 72-byte value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StandardInformation {
    times: FileTimes,
    attributes: FileAttributes,
    extended: Option<Extended>,
}

impl StandardInformation {
    /// Checks the attribute type and resident storage, then decodes its value.
    ///
    /// # Errors
    ///
    /// Returns [`ValueError`] for a type/storage mismatch or invalid payload length.
    pub fn from_attribute(attribute: Attribute<'_>) -> Result<Self, ValueError> {
        Self::parse(super::resident_value(
            attribute,
            AttributeType::STANDARD_INFORMATION,
        )?)
    }

    /// Decodes a raw standard-information payload, ignoring bytes after offset 0x48.
    ///
    /// # Errors
    ///
    /// Returns [`ValueError::InvalidStandardInformationLength`] unless the value
    /// is exactly 48 bytes or at least 72 bytes.
    pub fn parse(input: &[u8]) -> Result<Self, ValueError> {
        ensure!(
            input.len() == 48 || input.len() >= 72,
            InvalidStandardInformationLengthSnafu { len: input.len() }
        );
        let extended = if input.len() >= 72 {
            Some(Extended {
                owner_id: util::read_u32(input, 0x30).context(TruncatedValueSnafu)?,
                security_id: util::read_u32(input, 0x34).context(TruncatedValueSnafu)?,
                quota_charged: util::read_u64(input, 0x38).context(TruncatedValueSnafu)?,
                usn: util::read_u64(input, 0x40).context(TruncatedValueSnafu)?,
            })
        } else {
            None
        };
        Ok(Self {
            times: FileTimes::parse(input, 0)?,
            attributes: FileAttributes::from_raw(
                util::read_u32(input, 0x20).context(TruncatedValueSnafu)?,
            ),
            extended,
        })
    }
    /// Returns the four FILETIMEs at offsets 0x00..0x20.
    #[must_use]
    pub const fn times(self) -> FileTimes {
        self.times
    }
    /// Returns all file-attribute bits at offset 0x20.
    #[must_use]
    pub const fn file_attributes(self) -> FileAttributes {
        self.attributes
    }
    /// Returns the owner identifier at 0x30 for an extended value.
    #[must_use]
    pub fn owner_id(self) -> Option<u32> {
        self.extended.map(|extra| extra.owner_id)
    }
    /// Returns the security identifier at 0x34 for an extended value.
    #[must_use]
    pub fn security_id(self) -> Option<u32> {
        self.extended.map(|extra| extra.security_id)
    }
    /// Returns quota charged at 0x38 for an extended value.
    #[must_use]
    pub fn quota_charged(self) -> Option<u64> {
        self.extended.map(|extra| extra.quota_charged)
    }
    /// Returns the update sequence number at 0x40 for an extended value.
    #[must_use]
    pub fn usn(self) -> Option<u64> {
        self.extended.map(|extra| extra.usn)
    }
}
