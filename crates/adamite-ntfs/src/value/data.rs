//! One named or unnamed data stream without stream-selection policy.

use super::{ValueError, ensure_type};
use crate::{
    attribute::{Attribute, AttributeFlags, AttributeType, non_resident::NonResident},
    utf16::Utf16Le,
};

/// A DATA attribute preserving its individual name, flags and storage form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Data<'a> {
    attribute: Attribute<'a>,
}

impl<'a> Data<'a> {
    /// Checks DATA type and preserves either resident or non-resident storage.
    ///
    /// # Errors
    ///
    /// Returns [`ValueError::WrongAttributeType`] if the attribute is not DATA.
    pub fn from_attribute(attribute: Attribute<'a>) -> Result<Self, ValueError> {
        ensure_type(attribute, AttributeType::DATA)?;
        Ok(Self { attribute })
    }
    /// Returns the original stream name, empty for the unnamed stream.
    #[must_use]
    pub const fn name(self) -> Utf16Le<'a> {
        self.attribute.name()
    }
    /// Returns all stream flags, including unknown bits.
    #[must_use]
    pub const fn flags(self) -> AttributeFlags {
        self.attribute.flags()
    }
    /// Borrows resident stream bytes when this stream is resident.
    #[must_use]
    pub const fn resident_value(self) -> Option<&'a [u8]> {
        self.attribute.resident_value()
    }
    /// Returns the validated segment descriptor for non-resident storage.
    #[must_use]
    pub const fn non_resident(self) -> Option<NonResident<'a>> {
        self.attribute.non_resident()
    }
    /// Returns resident byte length or first-segment data size; continuation sizes are absent.
    #[must_use]
    pub fn logical_size(self) -> Option<u64> {
        match self.resident_value() {
            Some(value) => u64::try_from(value.len()).ok(),
            None => self
                .non_resident()
                .and_then(|descriptor| descriptor.sizes())
                .map(|sizes| sizes.data()),
        }
    }
}
