//! Public progress, phase and result types emitted by the scan driver.

use super::{ReadRequest, ScanStats};
use crate::snapshot::{PersistError, Snapshot};

/// A scan's current processing phase or terminal state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ScanPhase {
    /// Await boot-sector bytes.
    Boot,
    /// Read the first MFT record.
    Record0,
    /// Discover attribute lists and MFT extension segments.
    Discover,
    /// Stream and ingest MFT records.
    Stream,
    /// Merge extension contributions into their bases.
    Merge,
    /// Resolve parent and hard-link edges.
    Link,
    /// Build immutable snapshot storage from the flat parent-index array.
    Snapshot,
    /// Compute subtree totals.
    Aggregate,
    /// Freeze and order the completed tree.
    Finalize,
    /// Successful completion, with output delivered at most once.
    Done,
    /// Unrecoverable failure.
    Failed,
}

/// Current progress without performing reads or waiting for input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanProgress {
    /// Current phase.
    pub phase: ScanPhase,
    /// MFT records processed; boot processing leaves this zero.
    pub records_processed: u64,
    /// Issued requests not yet consumed, including responses already supplied.
    pub outstanding: usize,
    /// True when the caller must supply input before further work is possible.
    pub waiting_for_input: bool,
}

/// Owned result transferred exactly once by a completed scan.
#[derive(Debug, PartialEq, Eq)]
pub struct ScanOutput {
    /// Immutable tree storage.
    pub snapshot: Snapshot,
    /// Final record and diagnostic counters.
    pub stats: ScanStats,
}

impl ScanOutput {
    /// Encodes the complete snapshot and scan counters in canonical `.adm` v1 format.
    ///
    /// # Errors
    ///
    /// Returns [`PersistError`] if a format width is exceeded, length arithmetic
    /// overflows, or the output buffer cannot be allocated.
    pub fn to_bytes(&self) -> Result<Vec<u8>, PersistError> {
        self.snapshot.to_adm_bytes(&self.stats)
    }

    /// Decodes canonical `.adm` v1 bytes after validating the format, tree and indexes.
    ///
    /// # Errors
    ///
    /// Returns [`PersistError`] for invalid or truncated bytes, unsupported versions,
    /// checksum or snapshot invariant failures, arithmetic overflow, or allocation failure.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, PersistError> {
        let (snapshot, stats) = Snapshot::from_adm_bytes(bytes)?;
        Ok(Self { snapshot, stats })
    }
}

/// A read request, current progress or the completed scan result.
#[must_use = "handle the read request, progress or completed scan output"]
#[derive(Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ScanStep {
    /// A newly issued range; repeated polling does not reissue its identifier.
    Need(ReadRequest),
    /// CPU progress or a request for the caller to await its own I/O.
    Progress(ScanProgress),
    /// The one-time transfer of the completed output.
    Done(ScanOutput),
}
