//! Caller-driven phase transitions from boot bytes to an immutable scan output.

use super::{ConfigError, FeedError, ScanConfig, config::BOOT_BYTES, protocol::Requests};
use super::{ReadRequest, RequestId};
use crate::{
    ScanStats, Snapshot,
    error::ScanError,
    graph::entry::EntryRecord,
    graph::link::Linker,
    mft::{
        bootstrap::Record0,
        discovery::{Discovery, DiscoveryStep},
        ingest::Ingest,
        mapping::{MftMapping, RecordAssembly},
        merge::Merge,
    },
    snapshot::{Aggregator, Finalizer, ParentTree, SnapshotBuilder},
};
use adamite_ntfs::boot::VolumeGeometry;

/// Maximum streamed records decoded in one driver call across all ready responses.
const RECORDS_PER_STEP: usize = 256;

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

/// Streams, merges and resolves NTFS records from caller-supplied bytes.
///
/// Returns the completed immutable snapshot and diagnostics exactly once.
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
    snapshot_builder: Option<SnapshotBuilder>,
    aggregator: Option<Aggregator>,
    finalizer: Option<Finalizer>,
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
            snapshot_builder: None,
            aggregator: None,
            finalizer: None,
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
    /// terminates the scan. Calls after the completed output is delivered return Inactive.
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
                    let geometry = VolumeGeometry::from_boot_sector(&bytes)
                        .map_err(|source| ScanError::Geometry { source })?;
                    let mapping = MftMapping::record0(geometry)?;
                    let len = usize::try_from(geometry.file_record_size())
                        .map_err(|_| ScanError::overflow("file record size"))?;
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
            ScanPhase::Snapshot => self.snapshot(),
            ScanPhase::Aggregate => self.aggregate(),
            ScanPhase::Finalize => self.finalize(),
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
            let record0 = Record0::parse(assembly.into_bytes(), mapping.geometry)?;
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
        if state.is_reading()
            && let Some((_, bytes)) = self.requests.take_ready()
        {
            state.accept_bytes(&bytes)?;
        }
        match state.step(base)? {
            DiscoveryStep::Read => {
                if self.requests.len() < self.config.max_outstanding
                    && let Some(piece) = state.next_piece(base, self.config.chunk_size)?
                {
                    let request = self.requests.issue(piece.offset, piece.len)?;
                    state.planned(piece.len)?;
                    return Ok(ScanStep::Need(request));
                }
                Ok(self.progress(if self.requests.has_ready() {
                    InputStatus::Ready
                } else {
                    InputStatus::Waiting
                }))
            }
            DiscoveryStep::Progress => Ok(self.progress(InputStatus::Ready)),
            DiscoveryStep::Complete => {
                self.discovery = None;
                self.phase = ScanPhase::Stream;
                Ok(self.progress(InputStatus::Ready))
            }
        }
    }

    fn stream_transport_bytes(state: &Ingest, requests: &Requests) -> Result<usize, ScanError> {
        requests
            .retained_bytes()?
            .checked_add(state.buffer_capacity())
            .ok_or_else(|| ScanError::overflow("stream transport storage"))
    }

    fn check_stream_bound(
        state: &Ingest,
        requests: &Requests,
        config: &ScanConfig,
    ) -> Result<(), ScanError> {
        let bound = config
            .max_outstanding
            .checked_mul(config.chunk_size)
            .and_then(|bytes| bytes.checked_add(state.record_size()))
            .ok_or_else(|| ScanError::overflow("stream transport bound"))?;
        let retained = Self::stream_transport_bytes(state, requests)?;
        if retained > bound {
            return Err(ScanError::stream(
                "stream transport storage exceeds its bound",
            ));
        }
        Ok(())
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
        Self::check_stream_bound(state, &self.requests, &self.config)?;
        if self.requests.len() < self.config.max_outstanding
            && let Some(plan) = state.next_piece(&base.mapping, self.config.chunk_size)?
        {
            let request = self.requests.issue(plan.offset, plan.len)?;
            state.planned(plan);
            return Ok(ScanStep::Need(request));
        }
        let mut remaining = RECORDS_PER_STEP;
        while remaining != 0 {
            let Some(bytes) = self.requests.ready_bytes() else {
                break;
            };
            let consumed = state.accept_prefix(bytes)?;
            self.requests.consume_prefix(consumed)?;
            if state.finish_record(&base.mapping)? {
                remaining = remaining
                    .checked_sub(1)
                    .ok_or_else(|| ScanError::overflow("CPU record batch"))?;
            }
        }
        Self::check_stream_bound(state, &self.requests, &self.config)?;
        if state.is_complete()? {
            if state.has_partial_record() || self.requests.len() != 0 {
                return Err(ScanError::stream(
                    "leftover MFT transport after the final record",
                ));
            }
            self.phase = ScanPhase::Merge;
            return Ok(self.progress(InputStatus::Ready));
        }
        let can_issue = state.has_unplanned() && self.requests.len() < self.config.max_outstanding;
        Ok(self.progress(if !self.requests.has_ready() && !can_issue {
            InputStatus::Waiting
        } else {
            InputStatus::Ready
        }))
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

    fn snapshot(&mut self) -> Result<ScanStep, ScanError> {
        if self.snapshot_builder.is_none() {
            let tree = self.parent_tree.take().ok_or(ScanError::Link {
                reason: "missing parent tree",
            })?;
            let geometry = self
                .bootstrap
                .as_ref()
                .ok_or(ScanError::Link {
                    reason: "missing volume geometry",
                })?
                .mapping
                .geometry;
            self.snapshot_builder =
                Some(SnapshotBuilder::new(tree, geometry, &self.config.context)?);
            return Ok(self.progress(InputStatus::Ready));
        }
        let builder = self.snapshot_builder.as_mut().ok_or(ScanError::Link {
            reason: "missing snapshot builder",
        })?;
        if let Some(snapshot) = builder.step()? {
            let stats = self
                .ingest
                .as_ref()
                .ok_or(ScanError::Link {
                    reason: "missing scan statistics",
                })?
                .stats
                .clone();
            self.output = Some(ScanOutput { snapshot, stats });
            self.snapshot_builder = None;
            self.phase = ScanPhase::Aggregate;
        }
        Ok(self.progress(InputStatus::Ready))
    }

    fn aggregate(&mut self) -> Result<ScanStep, ScanError> {
        let output = self.output.as_mut().ok_or(ScanError::Link {
            reason: "missing snapshot output",
        })?;
        if self.aggregator.is_none() {
            self.aggregator = Some(Aggregator::new(&output.snapshot)?);
            return Ok(self.progress(InputStatus::Ready));
        }
        if self
            .aggregator
            .as_mut()
            .ok_or(ScanError::Link {
                reason: "missing aggregation state",
            })?
            .step(&mut output.snapshot)?
        {
            self.aggregator = None;
            self.phase = ScanPhase::Finalize;
        }
        Ok(self.progress(InputStatus::Ready))
    }

    fn finalize(&mut self) -> Result<ScanStep, ScanError> {
        let output = self.output.as_mut().ok_or(ScanError::Link {
            reason: "missing snapshot output",
        })?;
        if self.finalizer.is_none() {
            self.finalizer = Some(Finalizer::new());
            return Ok(self.progress(InputStatus::Ready));
        }
        if self
            .finalizer
            .as_mut()
            .ok_or(ScanError::Link {
                reason: "missing finalization state",
            })?
            .step(&mut output.snapshot)?
        {
            self.finalizer = None;
            self.phase = ScanPhase::Done;
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
        self.snapshot_builder = None;
        self.aggregator = None;
        self.finalizer = None;
    }
}

#[cfg(test)]
mod tests;
