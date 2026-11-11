//! Discovery of fixed and removable NTFS drive-letter volumes on Windows.

mod ffi;

use snafu::Snafu;
use std::{collections::TryReserveError, io};

/// Failure to enumerate logical drives or reserve discovery storage.
#[derive(Debug, Snafu)]
#[non_exhaustive]
pub enum VolumeListError {
    /// The Windows logical-drive enumeration failed or returned invalid data.
    #[snafu(display("cannot list logical drives: {source}"))]
    Query {
        /// Host enumeration failure.
        source: io::Error,
    },
    /// Discovery storage could not be reserved.
    #[snafu(display("volume listing allocation failed: {source}"))]
    Allocation {
        /// Underlying allocation failure.
        source: TryReserveError,
    },
}

impl From<TryReserveError> for VolumeListError {
    fn from(source: TryReserveError) -> Self {
        Self::Allocation { source }
    }
}

/// An accessible fixed or removable NTFS volume identified by a drive letter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Volume {
    letter: char,
    label: String,
    capacity: u64,
}

impl Volume {
    /// Lists accessible fixed and removable NTFS volumes in ascending drive-letter order.
    ///
    /// Drives whose filesystem, label or capacity cannot be queried are skipped, including empty removable drives.
    ///
    /// # Errors
    /// Returns [`VolumeListError`] if logical-drive enumeration or storage reservation fails.
    pub fn list() -> Result<Vec<Self>, VolumeListError> {
        ffi::list()
    }

    /// Returns the uppercase drive letter without a colon.
    #[must_use]
    pub const fn letter(&self) -> char {
        self.letter
    }

    /// Borrows the volume label, which may be empty.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Returns total capacity in bytes, including used and free space.
    #[must_use]
    pub const fn capacity(&self) -> u64 {
        self.capacity
    }
}
