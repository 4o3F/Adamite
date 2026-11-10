//! Typed failures of the snapshot byte format.

use adamite_ntfs::boot::BootError;
use snafu::Snafu;
use std::{collections::TryReserveError, str::Utf8Error};

/// Failure to encode or reconstruct a snapshot in the `.adm` byte format.
#[must_use]
#[derive(Debug, Snafu)]
#[non_exhaustive]
pub enum PersistError {
    /// The file does not start with the format magic.
    #[snafu(display("invalid snapshot magic"))]
    BadMagic,
    /// The format version is not supported.
    #[snafu(display("unsupported snapshot format version {version}"))]
    UnsupportedVersion {
        /// Version read from the header.
        version: u32,
    },
    /// The supplied bytes end before a required range.
    #[snafu(display("snapshot needs {needed} bytes but only {available} are available"))]
    Truncated {
        /// Required byte length.
        needed: usize,
        /// Supplied byte length.
        available: usize,
    },
    /// A header field violates the format contract.
    #[snafu(display("invalid snapshot header: {reason}"))]
    InvalidHeader {
        /// Violated header rule.
        reason: &'static str,
    },
    /// The section table violates the format contract.
    #[snafu(display("invalid snapshot section table: {reason}"))]
    InvalidSectionTable {
        /// Violated table rule.
        reason: &'static str,
    },
    /// The stored checksum differs from the computed checksum.
    #[snafu(display("snapshot checksum mismatch: expected {expected:#010x}, got {actual:#010x}"))]
    ChecksumMismatch {
        /// Checksum stored in the header.
        expected: u32,
        /// Checksum computed from the bytes.
        actual: u32,
    },
    /// A section violates its value or length rules.
    #[snafu(display("invalid snapshot section {section}: {reason}"))]
    InvalidSection {
        /// Numeric section kind.
        section: u32,
        /// Violated section rule.
        reason: &'static str,
    },
    /// A node violates a tree invariant.
    #[snafu(display("invalid snapshot tree at node {node}: {reason}"))]
    InvalidTree {
        /// Violated tree rule.
        reason: &'static str,
        /// Arena index of the affected node.
        node: u32,
    },
    /// A stored ranking or extension summary is inconsistent.
    #[snafu(display("invalid snapshot index: {reason}"))]
    InvalidIndex {
        /// Violated index rule.
        reason: &'static str,
    },
    /// Stored geometry fails NTFS geometry validation.
    #[snafu(display("invalid snapshot geometry: {source}"))]
    Geometry {
        /// Underlying geometry failure.
        source: BootError,
    },
    /// A text field is not valid UTF-8.
    #[snafu(display("invalid UTF-8 in snapshot section {section}: {source}"))]
    Utf8 {
        /// Numeric section kind containing the text.
        section: u32,
        /// Underlying UTF-8 failure.
        source: Utf8Error,
    },
    /// A checked size or offset calculation overflowed.
    #[snafu(display("overflow computing snapshot {operation}"))]
    Overflow {
        /// Calculation that could not be represented.
        operation: &'static str,
    },
    /// Fallible output or column reservation failed.
    #[snafu(display("snapshot persistence allocation failed: {source}"))]
    Allocation {
        /// Underlying allocation failure.
        source: TryReserveError,
    },
    /// An encoding value exceeds a format field's width.
    #[snafu(display("snapshot {operation} exceeds the format width"))]
    TooLarge {
        /// Value being represented.
        operation: &'static str,
    },
}

impl From<TryReserveError> for PersistError {
    fn from(source: TryReserveError) -> Self {
        Self::Allocation { source }
    }
}
