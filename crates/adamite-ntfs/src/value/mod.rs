//! Typed attribute payload decoders and shared validation errors.

pub mod attribute_list;
pub mod bitmap;
pub mod data;
pub mod file_name;
pub mod index_allocation;
pub mod reparse_point;
pub mod standard_information;

use snafu::{OptionExt, Snafu, ensure};

use crate::attribute::{Attribute, AttributeType};

fn ensure_type(attribute: Attribute<'_>, expected: AttributeType) -> Result<(), ValueError> {
    ensure!(
        attribute.attribute_type() == expected,
        WrongAttributeTypeSnafu {
            expected: expected.raw(),
            actual: attribute.attribute_type().raw()
        }
    );
    Ok(())
}

fn resident_value(attribute: Attribute<'_>, expected: AttributeType) -> Result<&[u8], ValueError> {
    ensure_type(attribute, expected)?;
    attribute.resident_value().context(WrongStorageFormSnafu)
}

/// Typed attribute payload validation failure.
///
/// `ScanStats` mapping: `WrongAttributeType` and `WrongStorageForm` count as
/// attribute type/storage mismatches; other variants count as corrupt typed
/// values. The core owns these counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Snafu)]
#[non_exhaustive]
pub enum ValueError {
    /// The attribute type differs from the requested typed decoder.
    #[snafu(display("attribute type {actual:#x} differs from expected {expected:#x}"))]
    WrongAttributeType {
        /// Required type code.
        expected: u32,
        /// Supplied type code.
        actual: u32,
    },
    /// The attribute uses a storage form unsupported by this decoder.
    #[snafu(display("unsupported attribute storage form"))]
    WrongStorageForm,
    /// A fixed field is incomplete.
    #[snafu(display("truncated typed attribute value"))]
    TruncatedValue,
    /// Standard information is neither 48 bytes nor at least 72 bytes.
    #[snafu(display("invalid standard-information length {len}"))]
    InvalidStandardInformationLength {
        /// Supplied value length.
        len: usize,
    },
    /// The file name is empty or its declared units exceed the value.
    #[snafu(display("invalid file-name length"))]
    InvalidNameLength,
    /// The file-name namespace is outside 0..=3.
    #[snafu(display("invalid file-name namespace {namespace}"))]
    InvalidNamespace {
        /// Raw namespace byte.
        namespace: u8,
    },
    /// A reparse value's declared data extends beyond its supplied bytes.
    #[snafu(display("invalid reparse data length"))]
    InvalidReparseLength,
    /// A reparse value exceeds 16 KiB.
    #[snafu(display("reparse value length {len} exceeds 16384 bytes"))]
    ReparseValueTooLarge {
        /// Supplied value length.
        len: usize,
    },
}
