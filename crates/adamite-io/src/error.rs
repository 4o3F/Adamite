//! Host, scan and driver protocol failures with their original sources.

use adamite_core::{
    error::ScanError,
    scan::{ConfigError, FeedError, ReadRequest, ScanPhase},
};
use snafu::Snafu;
use std::{collections::TryReserveError, io, path::PathBuf};

/// Failure to open or read a volume, configure a scan or drive it to completion.
#[derive(Debug, Snafu)]
#[non_exhaustive]
#[snafu(visibility(pub(crate)))]
pub enum VolumeError {
    /// Opening the source failed.
    #[snafu(display("failed to open {}: {source}", path.display()))]
    Open {
        /// Source path.
        path: PathBuf,
        /// Host open failure.
        source: io::Error,
    },
    /// The host denied access to the source.
    #[snafu(display("access denied to {}: {source}", path.display()))]
    AccessDenied {
        /// Source path.
        path: PathBuf,
        /// Host access failure.
        source: io::Error,
    },
    /// The drive identifier is not an ASCII letter.
    #[snafu(display("invalid drive letter {letter:?}"))]
    InvalidDrive {
        /// Rejected drive letter.
        letter: char,
    },
    /// A host volume query failed.
    #[snafu(display("volume query {operation} failed: {source}"))]
    Query {
        /// Failed query.
        operation: &'static str,
        /// Host query failure.
        source: io::Error,
    },
    /// Reported sector alignment is unsupported.
    #[snafu(display("unsupported sector alignment: logical {logical}, physical {physical}"))]
    UnsupportedAlignment {
        /// Logical sector size in bytes.
        logical: u32,
        /// Physical sector size in bytes.
        physical: u32,
    },
    /// The source length is not a multiple of its logical sector size.
    #[snafu(display("volume length {volume_len} is not aligned to logical sector size {logical}"))]
    UnalignedVolume {
        /// Source length in bytes.
        volume_len: u64,
        /// Logical sector size in bytes.
        logical: u32,
    },
    /// A positional read failed.
    #[snafu(display("read at {offset} with length {len} failed: {source}"))]
    Read {
        /// Original volume-relative byte offset.
        offset: u64,
        /// Original requested byte count.
        len: usize,
        /// Host read failure.
        source: io::Error,
    },
    /// A read returned fewer bytes than requested.
    #[snafu(display("short read at {offset}: expected {expected} bytes, received {actual}"))]
    ShortRead {
        /// Original volume-relative byte offset.
        offset: u64,
        /// Original requested byte count.
        expected: usize,
        /// Bytes actually read.
        actual: usize,
    },
    /// A requested byte range exceeds the source.
    #[snafu(display("read at {offset} with length {len} exceeds volume length {volume_len}"))]
    OutOfBounds {
        /// Requested volume-relative byte offset.
        offset: u64,
        /// Requested byte count.
        len: usize,
        /// Source length in bytes.
        volume_len: u64,
    },
    /// A calculation or integer conversion could not be represented.
    #[snafu(display("overflow computing {operation}"))]
    Overflow {
        /// Failed calculation.
        operation: &'static str,
    },
    /// Driver or reader storage could not be allocated.
    #[snafu(display("volume allocation failed: {source}"))]
    Allocation {
        /// Reservation failure.
        source: TryReserveError,
    },
    /// Scanner configuration is invalid.
    #[snafu(display("invalid scan configuration: {source}"))]
    Config {
        /// Core configuration failure.
        source: ConfigError,
    },
    /// The scanner encountered a fatal error.
    #[snafu(display("volume scan failed: {source}"))]
    Scan {
        /// Core scan failure.
        source: ScanError,
    },
    /// The scanner rejected an exact response.
    #[snafu(display("failed to feed request {request:?}: {source}"))]
    Feed {
        /// Request whose response was rejected.
        request: ReadRequest,
        /// Core response failure.
        source: FeedError,
    },
    /// Scanner steps violated the driver's request protocol.
    #[snafu(display(
        "scan protocol error in {phase:?} with {outstanding} outstanding requests: {reason}"
    ))]
    Protocol {
        /// Violated protocol condition.
        reason: &'static str,
        /// Most recently reported scan phase, when available.
        phase: Option<ScanPhase>,
        /// Outstanding request count at the failure.
        outstanding: usize,
    },
}
