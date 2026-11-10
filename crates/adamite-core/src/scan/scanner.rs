//! Caller-driven boot decoding and record-zero assembly.

use super::{ConfigError, FeedError, ScanConfig, config::BOOT_BYTES, protocol::Requests};
use super::{ReadRequest, RequestId};
use super::{
    bootstrap::{self, Record0},
    discovery::{Discovery, DiscoveryStep},
    ingest::{Ingest, IngestStep},
    mapping::{MftMapping, RecordAssembly, overflow},
    merge::Merge,
};
use crate::{
    ScanStats, Snapshot,
    entry::EntryRecord,
    tree::link::{Linker, ParentTree},
};
use adamite_ntfs::{
    AttributeError, AttributeListError, FileRefError, GeometryError, RecordError, RunlistError,
    fixup::FixupError, parse_boot_sector,
};
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
    /// Invalid volume geometry or MFT location in the boot sector.
    #[snafu(display("invalid volume geometry: {source}"))]
    Geometry {
        /// Parser geometry failure.
        source: GeometryError,
    },
    /// Bootstrap record update-sequence protection could not be restored.
    #[snafu(display("invalid bootstrap fixup: {source}"))]
    Fixup {
        /// Parser fixup failure.
        source: FixupError,
    },
    /// A fixed bootstrap record header is invalid.
    #[snafu(display("invalid bootstrap record: {source}"))]
    Record {
        /// Parser record failure.
        source: RecordError,
    },
    /// A bootstrap record contains an invalid attribute header.
    #[snafu(display("invalid bootstrap attribute: {source}"))]
    Attribute {
        /// Parser attribute failure.
        source: AttributeError,
    },
    /// MFT mapping pairs are malformed or terminate with incorrect coverage.
    #[snafu(display("invalid bootstrap runlist: {source}"))]
    Runlist {
        /// Parser runlist failure.
        source: RunlistError,
    },
    /// Attribute-list value or entry decoding failed.
    #[snafu(display("invalid bootstrap attribute list: {source}"))]
    AttributeList {
        /// Parser attribute-list failure.
        source: AttributeListError,
    },
    /// A non-resident list advertises more than the parser's 256 KiB bound.
    #[snafu(display("attribute-list size {size} exceeds 262144 bytes"))]
    AttributeListTooLarge {
        /// Advertised logical value size.
        size: u64,
    },
    /// A discovery pass found no reachable work and bootstrap remains incomplete.
    #[snafu(display("MFT bootstrap is incomplete and discovery cannot progress"))]
    BootstrapIncomplete,
    /// A streamed record ordinal cannot be represented by an NTFS reference.
    #[snafu(display("unrepresentable streamed file reference: {source}"))]
    FileRef {
        /// Parser reference construction failure.
        source: FileRefError,
    },
    /// A resolved entry or parent-index array invariant is invalid.
    #[snafu(display("invalid tree links: {reason}"))]
    Link {
        /// Rejected entry or parent-graph invariant.
        reason: &'static str,
    },
    /// A required filesystem record was absent or corrupt.
    #[snafu(display("required MFT record {record} is missing or corrupt"))]
    RequiredRecord {
        /// Required virtual record ordinal.
        record: u64,
    },
    /// An internal streaming invariant failed.
    #[snafu(display("invalid MFT stream: {reason}"))]
    Stream {
        /// Violated transport or assembly invariant.
        reason: &'static str,
    },
    /// Required record-zero identity, stream or mapping invariants failed.
    #[snafu(display("invalid MFT bootstrap: {reason}"))]
    Bootstrap {
        /// Rejected bootstrap invariant.
        reason: &'static str,
    },
    /// Arithmetic or index conversion overflowed before issuing a read.
    #[snafu(display("overflow computing {operation}"))]
    Overflow {
        /// Calculation that could not be represented.
        operation: &'static str,
    },
    /// A nonempty physical run exceeds the complete volume clusters.
    #[snafu(display("MFT run at LCN {lcn} with {clusters} clusters is outside the volume"))]
    OutOfVolume {
        /// Physical starting cluster.
        lcn: u64,
        /// Physical cluster count.
        clusters: u64,
    },
    /// A byte offset is not covered by the currently known MFT mapping.
    #[snafu(display("unmapped MFT byte offset {offset}"))]
    UnmappedRange {
        /// Missing virtual or physical byte offset.
        offset: u64,
    },
    /// Scan storage could not be allocated.
    #[snafu(display("scan allocation failed: {source}"))]
    Allocation {
        /// Allocation failure reported by the slot queue.
        source: TryReserveError,
    },
}

/// Streams, merges and resolves NTFS records from caller-supplied bytes.
///
/// Stops explicitly before snapshot storage construction, which is not yet implemented.
#[derive(Debug)]
pub struct Scanner {
    config: ScanConfig,
    requests: Requests,
    phase: ScanPhase,
    output: Option<ScanOutput>,
    record_read: Option<(MftMapping, RecordAssembly)>,
    bootstrap: Option<Record0>,
    discovery: Option<Discovery>,
    ingest: Option<Ingest>,
    merge: Option<Merge>,
    entries: Vec<EntryRecord>,
    linker: Option<Linker>,
    parent_tree: Option<ParentTree>,
}

#[derive(Clone, Copy)]
enum InputStatus {
    Ready,
    Waiting,
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
            record_read: None,
            bootstrap: None,
            discovery: None,
            ingest: None,
            merge: None,
            entries: Vec::new(),
            linker: None,
            parent_tree: None,
        })
    }

    /// Borrows the validated transport bounds and caller metadata.
    #[must_use]
    pub fn config(&self) -> &ScanConfig {
        &self.config
    }

    /// Advances one bounded assembly step or emits one new byte-range request.
    ///
    /// # Errors
    ///
    /// Invalid geometry, bootstrap data, arithmetic overflow or allocation failure
    /// terminates the scan. Successful tree linking currently terminates with
    /// [`ScanError::PhaseNotImplemented`] for Snapshot. Further calls return Inactive.
    pub fn step(&mut self) -> Result<ScanStep, ScanError> {
        let span = tracing::info_span!("scan_phase", phase = ?self.phase);
        let _entered = span.enter();
        let result = self.advance();
        if result.is_err() && !matches!(self.phase, ScanPhase::Done | ScanPhase::Failed) {
            self.fail();
        }
        result
    }

    fn advance(&mut self) -> Result<ScanStep, ScanError> {
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
                if let Some((_, bytes)) = self.requests.take_ready() {
                    let geometry = parse_boot_sector(&bytes)
                        .map_err(|source| ScanError::Geometry { source })?;
                    let mapping = MftMapping::record0(geometry)?;
                    let len = usize::try_from(geometry.file_record_size())
                        .map_err(|_| overflow("file record size"))?;
                    self.record_read = Some((mapping, RecordAssembly::new(0, len)?));
                    self.phase = ScanPhase::Record0;
                    return Ok(self.progress(InputStatus::Ready));
                }
                if self.requests.len() == 0 {
                    return self.requests.issue(0, BOOT_BYTES).map(ScanStep::Need);
                }
                Ok(self.progress(InputStatus::Waiting))
            }
            ScanPhase::Record0 => self.read_record0(),
            ScanPhase::Discover => self.discover(),
            ScanPhase::Stream => self.stream(),
            ScanPhase::Merge => self.merge(),
            ScanPhase::Link => self.link(),
            phase => Err(ScanError::PhaseNotImplemented { phase }),
        }
    }

    fn read_record0(&mut self) -> Result<ScanStep, ScanError> {
        let (mapping, assembly) = self.record_read.as_mut().ok_or(ScanError::Bootstrap {
            reason: "missing record-zero assembly",
        })?;
        if let Some((_, bytes)) = self.requests.take_ready() {
            assembly.append(&bytes)?;
        }
        if assembly.complete() {
            let (mapping, assembly) = self.record_read.take().ok_or(ScanError::Bootstrap {
                reason: "missing completed record-zero assembly",
            })?;
            let record0 = bootstrap::parse_record0(assembly.into_bytes(), mapping.geometry)?;
            tracing::debug!(
                reference = ?record0.reference,
                data_id = record0.data_id,
                data_size = record0.data_size,
                runs = record0.mapping.runs.len(),
                attribute_lists = ?record0.attribute_lists,
                "record-zero bootstrap ready"
            );
            self.bootstrap = Some(record0);
            self.phase = ScanPhase::Discover;
            return Ok(self.progress(InputStatus::Ready));
        }
        if self.requests.len() < self.config.max_outstanding
            && let Some(piece) = assembly.next_piece(mapping, self.config.chunk_size)?
        {
            let request = self.requests.issue(piece.offset, piece.len)?;
            assembly.planned(piece.len)?;
            return Ok(ScanStep::Need(request));
        }
        Ok(self.progress(if self.requests.has_ready() {
            InputStatus::Ready
        } else {
            InputStatus::Waiting
        }))
    }

    fn discover(&mut self) -> Result<ScanStep, ScanError> {
        let base = self.bootstrap.as_mut().ok_or(ScanError::Bootstrap {
            reason: "missing record-zero bootstrap",
        })?;
        if self.discovery.is_none() {
            self.discovery = Some(Discovery::new(base)?);
            return Ok(self.progress(InputStatus::Ready));
        }
        let state = self.discovery.as_mut().ok_or(ScanError::Bootstrap {
            reason: "missing discovery state",
        })?;
        match state.step(base, &mut self.requests, &self.config)? {
            DiscoveryStep::Need(request) => Ok(ScanStep::Need(request)),
            DiscoveryStep::Progress { waiting } => Ok(self.progress(if waiting {
                InputStatus::Waiting
            } else {
                InputStatus::Ready
            })),
            DiscoveryStep::Complete => {
                self.discovery = None;
                self.phase = ScanPhase::Stream;
                Ok(self.progress(InputStatus::Ready))
            }
        }
    }

    fn stream(&mut self) -> Result<ScanStep, ScanError> {
        let base = self.bootstrap.as_ref().ok_or(ScanError::Stream {
            reason: "missing complete MFT mapping",
        })?;
        if self.ingest.is_none() {
            self.ingest = Some(Ingest::new(
                base.data_size,
                base.mapping.geometry.file_record_size(),
            )?);
            return Ok(self.progress(InputStatus::Ready));
        }
        let state = self.ingest.as_mut().ok_or(ScanError::Stream {
            reason: "missing ingestion state",
        })?;
        match state.step(&base.mapping, &mut self.requests, &self.config)? {
            IngestStep::Need(request) => Ok(ScanStep::Need(request)),
            IngestStep::Progress { waiting } => Ok(self.progress(if waiting {
                InputStatus::Waiting
            } else {
                InputStatus::Ready
            })),
            IngestStep::Complete => {
                self.phase = ScanPhase::Merge;
                Ok(self.progress(InputStatus::Ready))
            }
        }
    }

    fn merge(&mut self) -> Result<ScanStep, ScanError> {
        let ingest = self.ingest.as_mut().ok_or(ScanError::Stream {
            reason: "missing ingestion handoff",
        })?;
        if self.merge.is_none() {
            let cluster_size = self
                .bootstrap
                .as_ref()
                .ok_or(ScanError::Stream {
                    reason: "missing volume geometry",
                })?
                .mapping
                .geometry
                .cluster_size();
            self.merge = Some(Merge::new(
                std::mem::take(&mut ingest.slots),
                std::mem::take(&mut ingest.patches),
                cluster_size,
            ));
            return Ok(self.progress(InputStatus::Ready));
        }
        let merge = self.merge.as_mut().ok_or(ScanError::Stream {
            reason: "missing merge state",
        })?;
        if merge.step(&mut ingest.stats)? {
            self.entries = std::mem::take(&mut merge.entries);
            self.merge = None;
            self.phase = ScanPhase::Link;
        }
        Ok(self.progress(InputStatus::Ready))
    }

    fn link(&mut self) -> Result<ScanStep, ScanError> {
        if self.linker.is_none() {
            self.linker = Some(Linker::new(std::mem::take(&mut self.entries))?);
            return Ok(self.progress(InputStatus::Ready));
        }
        let stats = &mut self
            .ingest
            .as_mut()
            .ok_or(ScanError::Link {
                reason: "missing scan statistics",
            })?
            .stats;
        let linker = self.linker.as_mut().ok_or(ScanError::Link {
            reason: "missing linker state",
        })?;
        if linker.step(stats)? {
            self.parent_tree = Some(std::mem::take(&mut linker.result));
            self.linker = None;
            self.phase = ScanPhase::Snapshot;
        }
        Ok(self.progress(InputStatus::Ready))
    }

    fn progress(&self, input: InputStatus) -> ScanStep {
        ScanStep::Progress(ScanProgress {
            phase: self.phase,
            records_processed: self
                .ingest
                .as_ref()
                .map_or(0, |state| state.stats.records_total),
            outstanding: self.requests.len(),
            waiting_for_input: matches!(input, InputStatus::Waiting),
        })
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
        self.record_read = None;
        self.discovery = None;
        self.merge = None;
        self.linker = None;
    }
}

#[cfg(test)]
mod tests;
