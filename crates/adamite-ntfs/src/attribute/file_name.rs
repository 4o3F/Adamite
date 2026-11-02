//! File-name metadata with borrowed original UTF-16LE units.

use super::{
    Attribute, AttributeType, FileAttributes, FileTimes, InvalidNameLengthSnafu,
    InvalidNamespaceSnafu, TruncatedValueSnafu, ValueError,
};
use crate::{FileRef, Utf16Le, bytes};
use snafu::{OptionExt, ensure};

/// One file-name value with cached sizes and its original namespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileName<'a> {
    parent: FileRef,
    times: FileTimes,
    allocated: u64,
    data: u64,
    attributes: FileAttributes,
    ea_or_reparse: u32,
    namespace: u8,
    name: Utf16Le<'a>,
}

impl<'a> FileName<'a> {
    /// Checks FILE_NAME type and resident storage before decoding the value.
    ///
    /// # Errors
    ///
    /// Returns [`ValueError`] for a type/storage mismatch or invalid file-name value.
    pub fn from_attribute(attribute: Attribute<'a>) -> Result<Self, ValueError> {
        Self::parse(super::resident_value(attribute, AttributeType::FILE_NAME)?)
    }
    /// Decodes one raw file-name value, ignoring bytes after its declared name.
    ///
    /// # Errors
    ///
    /// Checks fixed extent, namespace 0..=3, then a nonempty declared name within
    /// the value. Trailing bytes are ignored.
    pub fn parse(input: &'a [u8]) -> Result<Self, ValueError> {
        input.get(..0x42).context(TruncatedValueSnafu)?;
        let namespace = bytes::read_u8(input, 0x41).context(TruncatedValueSnafu)?;
        ensure!(namespace <= 3, InvalidNamespaceSnafu { namespace });
        let count = bytes::read_u8(input, 0x40).context(TruncatedValueSnafu)?;
        ensure!(count > 0, InvalidNameLengthSnafu);
        let range =
            bytes::scaled_range(0x42, usize::from(count), 2).context(InvalidNameLengthSnafu)?;
        let name = Utf16Le::new(input.get(range).context(InvalidNameLengthSnafu)?)
            .map_err(|_| InvalidNameLengthSnafu.build())?;
        Ok(Self {
            parent: FileRef::from_raw(bytes::read_u64(input, 0).context(TruncatedValueSnafu)?),
            times: FileTimes::parse(input, 8)?,
            allocated: bytes::read_u64(input, 0x28).context(TruncatedValueSnafu)?,
            data: bytes::read_u64(input, 0x30).context(TruncatedValueSnafu)?,
            attributes: FileAttributes::from_raw(
                bytes::read_u32(input, 0x38).context(TruncatedValueSnafu)?,
            ),
            ea_or_reparse: bytes::read_u32(input, 0x3c).context(TruncatedValueSnafu)?,
            namespace,
            name,
        })
    }
    /// Returns the parent reference at value offset 0x00.
    pub const fn parent(self) -> FileRef {
        self.parent
    }
    /// Returns the cached FILETIMEs at offsets 0x08..0x28.
    pub const fn times(self) -> FileTimes {
        self.times
    }
    /// Returns cached allocated bytes at 0x28, which may be stale.
    pub const fn cached_allocated_size(self) -> u64 {
        self.allocated
    }
    /// Returns cached data bytes at 0x30, which may be stale.
    pub const fn cached_data_size(self) -> u64 {
        self.data
    }
    /// Returns all file-attribute bits at offset 0x38.
    pub const fn file_attributes(self) -> FileAttributes {
        self.attributes
    }
    /// Returns the uninterpreted EA-size/reparse-tag word at offset 0x3C.
    pub const fn ea_size_or_reparse_tag(self) -> u32 {
        self.ea_or_reparse
    }
    /// Returns namespace 0 (POSIX), 1 (Win32), 2 (DOS), or 3 (Win32 and DOS).
    pub const fn namespace(self) -> u8 {
        self.namespace
    }
    /// Borrows the original name units at offset 0x42.
    pub const fn name(self) -> Utf16Le<'a> {
        self.name
    }
}
