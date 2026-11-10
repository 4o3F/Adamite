//! Reparse tags and opaque payloads without link-target interpretation.

use super::{
    Attribute, AttributeType, InvalidReparseLengthSnafu, ReparseValueTooLargeSnafu,
    TruncatedValueSnafu, ValueError,
};
use crate::bytes;
use snafu::{OptionExt, ensure};

/// A reparse tag, declared length and every byte following its eight-byte header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReparsePoint<'a> {
    tag: u32,
    data_length: u16,
    payload: &'a [u8],
}

impl<'a> ReparsePoint<'a> {
    /// Checks REPARSE_POINT type and resident storage, then decodes the payload.
    ///
    /// # Errors
    ///
    /// Returns [`ValueError`] for a type/storage mismatch or an invalid value extent.
    pub fn from_attribute(attribute: Attribute<'a>) -> Result<Self, ValueError> {
        Self::parse(super::resident_value(
            attribute,
            AttributeType::REPARSE_POINT,
        )?)
    }
    /// Accepts 8..=16384 raw bytes and preserves unknown tags and trailing payload bytes.
    ///
    /// # Errors
    ///
    /// Checks the input extent, then the declared data length within that extent.
    pub fn parse(input: &'a [u8]) -> Result<Self, ValueError> {
        input.get(..8).context(TruncatedValueSnafu)?;
        ensure!(
            input.len() <= 16384,
            ReparseValueTooLargeSnafu { len: input.len() }
        );
        let data_length = bytes::read_u16(input, 4).context(TruncatedValueSnafu)?;
        let range = bytes::range(8, usize::from(data_length)).context(InvalidReparseLengthSnafu)?;
        ensure!(range.end <= input.len(), InvalidReparseLengthSnafu);
        Ok(Self {
            tag: bytes::read_u32(input, 0).context(TruncatedValueSnafu)?,
            data_length,
            payload: input.get(8..).context(TruncatedValueSnafu)?,
        })
    }
    /// Returns the uninterpreted u32 tag at value offset 0x00.
    #[must_use]
    pub const fn tag(self) -> u32 {
        self.tag
    }
    /// Returns the declared u16 data length at value offset 0x04.
    #[must_use]
    pub const fn data_length(self) -> u16 {
        self.data_length
    }
    /// Borrows every byte after offset eight, including bytes beyond the declared data length.
    #[must_use]
    pub const fn payload(self) -> &'a [u8] {
        self.payload
    }
}
