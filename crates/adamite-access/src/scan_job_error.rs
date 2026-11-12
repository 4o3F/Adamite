use crate::SourceError;
use adamite_core::snapshot::PersistError;
use adamite_io::elevation::ElevationError;
use snafu::Snafu;
use std::{io, path::PathBuf};

/// A failure to publish a snapshot, report status or run an elevated scan.
#[derive(Debug, Snafu)]
#[snafu(module)]
pub enum ScanJobError {
    /// The status destination already exists.
    #[snafu(display("status file {} already exists", path.display()))]
    StatusExists {
        /// Path involved in the failure.
        path: PathBuf,
    },
    /// Writing or reading scan status failed.
    #[snafu(display("cannot report scan status"))]
    Status {
        /// Underlying error.
        source: adamite_io::status::StatusError,
    },
    /// The user declined administrator access.
    #[snafu(display("elevation was declined"))]
    ElevationDeclined,
    /// Querying, launching or waiting for the elevated process failed.
    #[snafu(display("cannot run elevated scan"))]
    Elevation {
        /// Underlying error.
        source: adamite_io::elevation::ElevationError,
    },
    /// The child reported a failure and its exit code.
    #[snafu(display("{message}"))]
    ChildFailed {
        /// Message reported by the child.
        message: String,
        /// Exit code reported by the child.
        exit_code: i32,
    },
    /// The child exited without a final status line.
    #[snafu(display("elevated scan exited with code {exit_code} without a final status line"))]
    ChildExited {
        /// Exit code reported by the child.
        exit_code: u32,
    },
    /// File I/O failed.
    #[snafu(display("cannot {operation} {}", path.display()))]
    Io {
        /// Operation that failed.
        operation: &'static str,
        /// Path involved in the failure.
        path: PathBuf,
        /// Underlying error.
        source: io::Error,
    },
    /// Snapshot encoding failed.
    #[snafu(display("cannot {operation} snapshot {}", path.display()))]
    Persist {
        /// Operation that failed.
        operation: &'static str,
        /// Path involved in the failure.
        path: PathBuf,
        /// Underlying error.
        source: PersistError,
    },
    /// The snapshot destination already exists.
    #[snafu(display("output {} already exists; use --force to replace it", path.display()))]
    OutputExists {
        /// Path involved in the failure.
        path: PathBuf,
    },
    /// The snapshot destination is a directory.
    #[snafu(display("output {} is a directory", path.display()))]
    OutputIsDirectory {
        /// Path involved in the failure.
        path: PathBuf,
    },
    /// The filesystem cannot publish a hard link.
    #[snafu(display("cannot save {} without replacement: hard links are unsupported; use --force", path.display()))]
    UnsupportedNoClobber {
        /// Path involved in the failure.
        path: PathBuf,
        /// Underlying error.
        source: io::Error,
    },
    /// The snapshot destination resolves to the source image.
    #[snafu(display("output {} resolves to the source image", path.display()))]
    SameAsSource {
        /// Path involved in the failure.
        path: PathBuf,
    },
    /// The snapshot length exceeds this platform’s address space.
    #[snafu(display("snapshot {} is too large for this platform", path.display()))]
    SnapshotLength {
        /// Path involved in the failure.
        path: PathBuf,
    },
    /// Loading the completed snapshot failed.
    #[snafu(transparent)]
    Source {
        /// Source detection or loading error.
        source: SourceError,
    },
}

impl From<ElevationError> for ScanJobError {
    fn from(source: ElevationError) -> Self {
        if matches!(source, ElevationError::Declined) {
            Self::ElevationDeclined
        } else {
            Self::Elevation { source }
        }
    }
}
