//! Boot request driver and terminal output delivery.

use super::{ConfigError, FeedError, ScanConfig, config::BOOT_BYTES, protocol::Requests};
use super::{ReadRequest, RequestId};
use crate::{ScanStats, Snapshot};
use snafu::Snafu;
use std::collections::TryReserveError;

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

/// A read request, current progress or the completed scan result.
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

/// Failure to advance the scan or issue a valid request.
#[derive(Debug, Snafu)]
#[snafu(visibility(pub(crate)))]
#[non_exhaustive]
pub enum ScanError {
    /// A terminal scan cannot advance again.
    #[snafu(display("scan is inactive"))]
    Inactive,
    /// The next processing phase is not implemented yet.
    #[snafu(display("scan phase {phase:?} is not implemented"))]
    PhaseNotImplemented {
        /// Phase required to continue the scan.
        phase: ScanPhase,
    },
    /// All issued request slots remain occupied.
    #[snafu(display("outstanding request limit {max_outstanding} reached"))]
    OutstandingLimit {
        /// Configured number of slots.
        max_outstanding: usize,
    },
    /// A range is empty, exceeds the chunk bound or overflows a byte offset.
    #[snafu(display("invalid read range at {offset} with length {len}"))]
    InvalidReadRange {
        /// Requested volume-relative byte offset.
        offset: u64,
        /// Requested byte count.
        len: usize,
    },
    /// Every representable request identifier has been issued.
    #[snafu(display("request identifiers exhausted"))]
    RequestIdExhausted,
    /// Request-slot storage could not be allocated.
    #[snafu(display("request allocation failed: {source}"))]
    Allocation {
        /// Allocation failure reported by the slot queue.
        source: TryReserveError,
    },
}

/// Emits a boot-sector request and fails explicitly before record-zero bootstrap.
///
/// The caller performs all reads. Boot decoding and subsequent phases are added in M5.
#[derive(Debug)]
pub struct Scanner {
    config: ScanConfig,
    requests: Requests,
    phase: ScanPhase,
    output: Option<ScanOutput>,
}

impl Scanner {
    /// Validates configuration and creates a scan without allocating request buffers.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] for invalid capacity or chunk-size bounds.
    pub fn new(config: ScanConfig) -> Result<Self, ConfigError> {
        config.validate()?;
        Ok(Self {
            requests: Requests::new(config.max_outstanding, config.chunk_size),
            config,
            phase: ScanPhase::Boot,
            output: None,
        })
    }

    /// Borrows the validated transport bounds and caller metadata.
    pub fn config(&self) -> &ScanConfig {
        &self.config
    }

    /// Issues the boot request, reports waiting, or transfers a terminal result once.
    ///
    /// # Errors
    ///
    /// After consuming boot bytes, returns [`ScanError::PhaseNotImplemented`] for Record0
    /// and enters Failed. Further calls return [`ScanError::Inactive`]. Allocation or
    /// request-issuance failures also terminate the scan.
    pub fn step(&mut self) -> Result<ScanStep, ScanError> {
        let span = tracing::info_span!("scan_phase", phase = ?self.phase);
        let _entered = span.enter();
        match self.phase {
            ScanPhase::Done => {
                self.requests.deactivate();
                self.output
                    .take()
                    .map(ScanStep::Done)
                    .ok_or(ScanError::Inactive)
            }
            ScanPhase::Failed => Err(ScanError::Inactive),
            ScanPhase::Boot => {
                if self.requests.len() == 0 {
                    return match self.requests.issue(0, BOOT_BYTES) {
                        Ok(request) => Ok(ScanStep::Need(request)),
                        Err(error) => {
                            self.fail();
                            Err(error)
                        }
                    };
                }
                if self.requests.take_ready().is_some() {
                    // TODO(M5): decode boot bytes and plan the record-zero read.
                    self.fail();
                    return Err(ScanError::PhaseNotImplemented {
                        phase: ScanPhase::Record0,
                    });
                }
                Ok(ScanStep::Progress(ScanProgress {
                    phase: ScanPhase::Boot,
                    records_processed: 0,
                    outstanding: self.requests.len(),
                    waiting_for_input: true,
                }))
            }
            phase => {
                self.fail();
                Err(ScanError::PhaseNotImplemented { phase })
            }
        }
    }

    /// Copies an exact response into its identified pending slot, in any arrival order.
    ///
    /// # Errors
    ///
    /// Rejects inactive scans, unknown or consumed ids, duplicate responses, wrong
    /// lengths and allocation failures without changing scan state.
    pub fn feed(&mut self, id: RequestId, bytes: &[u8]) -> Result<(), FeedError> {
        if matches!(self.phase, ScanPhase::Done | ScanPhase::Failed) {
            return Err(FeedError::Inactive);
        }
        self.requests.feed(id, bytes)
    }

    fn fail(&mut self) {
        self.phase = ScanPhase::Failed;
        self.requests.deactivate();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;

    #[test]
    fn protocol_done_transfers_output_once_then_rejects_calls() -> Result<(), Box<dyn Error>> {
        let mut scanner = Scanner::new(ScanConfig::default())?;
        // Finalization is not implemented in M4; seed its terminal handoff directly.
        scanner.phase = ScanPhase::Done;
        scanner.output = Some(ScanOutput {
            snapshot: Snapshot::default(),
            stats: ScanStats::default(),
        });
        assert!(matches!(scanner.step()?, ScanStep::Done(_)));
        for _ in 0..3 {
            assert!(matches!(scanner.step(), Err(ScanError::Inactive)));
            assert!(matches!(
                scanner.feed(RequestId::from_raw(0), &[]),
                Err(FeedError::Inactive)
            ));
        }
        Ok(())
    }
}
