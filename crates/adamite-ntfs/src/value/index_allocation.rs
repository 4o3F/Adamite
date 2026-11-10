//! Non-resident index-allocation descriptors without INDX block decoding.

use super::{ValueError, WrongStorageFormSnafu, ensure_type};
use crate::{
    attribute::{Attribute, AttributeFlags, AttributeType, non_resident::NonResident},
    utf16::Utf16Le,
};
use snafu::OptionExt;

/// A named INDEX_ALLOCATION segment preserving its flags and stream descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexAllocation<'a> {
    name: Utf16Le<'a>,
    flags: AttributeFlags,
    descriptor: NonResident<'a>,
}

impl<'a> IndexAllocation<'a> {
    /// Checks INDEX_ALLOCATION type and requires non-resident storage.
    ///
    /// # Errors
    ///
    /// Returns [`ValueError`] for a wrong type or resident storage.
    pub fn from_attribute(attribute: Attribute<'a>) -> Result<Self, ValueError> {
        ensure_type(attribute, AttributeType::INDEX_ALLOCATION)?;
        Ok(Self {
            name: attribute.name(),
            flags: attribute.flags(),
            descriptor: attribute.non_resident().context(WrongStorageFormSnafu)?,
        })
    }
    /// Returns any original index name, normally `$I30`.
    #[must_use]
    pub const fn name(self) -> Utf16Le<'a> {
        self.name
    }
    /// Returns all index-stream flags.
    #[must_use]
    pub const fn flags(self) -> AttributeFlags {
        self.flags
    }
    /// Returns the non-resident segment without interpreting index blocks.
    #[must_use]
    pub const fn descriptor(self) -> NonResident<'a> {
        self.descriptor
    }
}
