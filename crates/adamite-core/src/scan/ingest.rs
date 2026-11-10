//! Bounded MFT transport, ordinal classification and owned record contributions.

mod decode;

use super::{
    ReadRequest, ScanConfig,
    bootstrap::push,
    mapping::{MftMapping, overflow},
    protocol::Requests,
    scanner::ScanError,
};
use crate::{
    ScanStats,
    entry::{Patch, RecordKind, RecordSlot},
};
use adamite_ntfs::record::{
    RecordClass, RecordError, RecordFlags, RecordHeader, fixup::FixupError, reference::FileRef,
    system::ROOT,
};

const RECORDS_PER_STEP: usize = 256;

pub(super) fn bump(counter: &mut u64) -> Result<(), ScanError> {
    *counter = counter
        .checked_add(1)
        .ok_or_else(|| overflow("scan counter"))?;
    Ok(())
}

pub(super) fn stream_error(reason: &'static str) -> ScanError {
    ScanError::Stream { reason }
}

#[derive(Debug)]
pub(super) enum IngestStep {
    Need(ReadRequest),
    Progress { waiting: bool },
    Complete,
}

#[derive(Debug)]
pub(super) struct Ingest {
    data_size: u64,
    record_size: usize,
    planned: u64,
    raw: Vec<u8>,
    pub slots: Vec<RecordSlot>,
    pub patches: Vec<Patch>,
    pub stats: ScanStats,
}

impl Ingest {
    pub(super) fn new(data_size: u64, record_size: u32) -> Result<Self, ScanError> {
        let record_size = usize::try_from(record_size).map_err(|_| overflow("record size"))?;
        let mut raw = Vec::new();
        raw.try_reserve_exact(record_size)
            .map_err(|source| ScanError::Allocation { source })?;
        Ok(Self {
            data_size,
            record_size,
            planned: 0,
            raw,
            slots: Vec::new(),
            patches: Vec::new(),
            stats: ScanStats::default(),
        })
    }

    pub(super) fn transport_bytes(&self, requests: &Requests) -> Result<usize, ScanError> {
        requests
            .retained_bytes()?
            .checked_add(self.raw.capacity())
            .ok_or_else(|| overflow("stream transport storage"))
    }

    fn check_bound(&self, requests: &Requests, config: &ScanConfig) -> Result<(), ScanError> {
        let bound = config
            .max_outstanding
            .checked_mul(config.chunk_size)
            .and_then(|bytes| bytes.checked_add(self.record_size))
            .ok_or_else(|| overflow("stream transport bound"))?;
        let retained = self.transport_bytes(requests)?;
        if retained > bound {
            return Err(stream_error("stream transport storage exceeds its bound"));
        }
        Ok(())
    }

    pub(super) fn step(
        &mut self,
        mapping: &MftMapping,
        requests: &mut Requests,
        config: &ScanConfig,
    ) -> Result<IngestStep, ScanError> {
        self.check_bound(requests, config)?;
        if self.planned < self.data_size && requests.len() < config.max_outstanding {
            let remaining = self
                .data_size
                .checked_sub(self.planned)
                .ok_or_else(|| overflow("unplanned MFT bytes"))?;
            let len = usize::try_from(
                remaining
                    .min(u64::try_from(config.chunk_size).map_err(|_| overflow("chunk length"))?),
            )
            .map_err(|_| overflow("stream request length"))?;
            let piece = mapping.read_piece(self.planned, len)?;
            let end = self
                .planned
                .checked_add(
                    u64::try_from(piece.len).map_err(|_| overflow("stream request length"))?,
                )
                .ok_or_else(|| overflow("stream planning cursor"))?;
            let request = requests.issue(piece.offset, piece.len)?;
            self.planned = end;
            return Ok(IngestStep::Need(request));
        }
        let mut processed = 0usize;
        while processed < RECORDS_PER_STEP {
            let Some(bytes) = requests.ready_bytes() else {
                break;
            };
            let remaining = self
                .record_size
                .checked_sub(self.raw.len())
                .ok_or_else(|| overflow("partial record remainder"))?;
            let len = remaining.min(bytes.len());
            self.raw.extend_from_slice(
                bytes
                    .get(..len)
                    .ok_or_else(|| stream_error("missing response prefix"))?,
            );
            requests.consume_prefix(len)?;
            if self.raw.len() == self.record_size {
                self.record(mapping)?;
                self.raw.clear();
                processed = processed
                    .checked_add(1)
                    .ok_or_else(|| overflow("CPU record batch"))?;
            }
        }
        self.check_bound(requests, config)?;
        let consumed = self
            .stats
            .records_total
            .checked_mul(u64::try_from(self.record_size).map_err(|_| overflow("record size"))?)
            .ok_or_else(|| overflow("consumed MFT bytes"))?;
        if consumed == self.data_size {
            if self
                .slots
                .get(usize::try_from(ROOT).map_err(|_| overflow("root slot index"))?)
                .is_none()
            {
                return Err(ScanError::RequiredRecord { record: ROOT });
            }
            if !self.raw.is_empty() || requests.len() != 0 {
                return Err(stream_error(
                    "leftover MFT transport after the final record",
                ));
            }
            return Ok(IngestStep::Complete);
        }
        let can_issue = self.planned < self.data_size && requests.len() < config.max_outstanding;
        Ok(IngestStep::Progress {
            waiting: !requests.has_ready() && !can_issue,
        })
    }

    fn record(&mut self, mapping: &MftMapping) -> Result<(), ScanError> {
        let ordinal = self.stats.records_total;
        bump(&mut self.stats.records_total)?;
        let class = RecordClass::classify(&mut self.raw);
        let mut slot = RecordSlot {
            classification: RecordKind::Corrupt,
            sequence: None,
            flags: None,
            base: None,
        };
        match class {
            RecordClass::Unused => {
                slot.classification = RecordKind::Unused;
                bump(&mut self.stats.unused)?;
            }
            RecordClass::Baad => {
                slot.classification = RecordKind::Baad;
                bump(&mut self.stats.baad)?;
            }
            RecordClass::NotInUse(header) => {
                slot.classification = RecordKind::Deleted;
                set_header(&mut slot, header);
                bump(&mut self.stats.deleted)?;
                // Unallocated FILE slots can leave the stored record number unset.
                if header
                    .record_number()
                    .is_some_and(|number| number != 0 && u64::from(number) != ordinal)
                {
                    bump(&mut self.stats.corrupt_records)?;
                    bump(&mut self.stats.corrupt_headers)?;
                }
            }
            RecordClass::Corrupt(error) => {
                bump(&mut self.stats.corrupt_records)?;
                record_error(error, &mut self.stats)?;
            }
            RecordClass::Base(record) | RecordClass::Extension(record) => {
                let header = record.header();
                let base = header.base_reference().is_null();
                slot.classification = if base {
                    RecordKind::Base
                } else {
                    RecordKind::Extension
                };
                set_header(&mut slot, header);
                bump(&mut self.stats.records_in_use)?;
                bump(if base {
                    &mut self.stats.base_records
                } else {
                    &mut self.stats.extension_records
                })?;
                if header
                    .record_number()
                    .is_some_and(|number| u64::from(number) != ordinal)
                {
                    bump(&mut self.stats.corrupt_records)?;
                    bump(&mut self.stats.corrupt_headers)?;
                } else {
                    let reference = match FileRef::new(ordinal, header.sequence()) {
                        Ok(reference) => reference,
                        Err(source) => {
                            bump(&mut self.stats.invalid_file_refs)?;
                            return Err(ScanError::FileRef { source });
                        }
                    };
                    match decode::contribution(&record, reference, mapping.geometry) {
                        Ok(contribution) if base => slot.base = Some(contribution),
                        Ok(contribution) => push(
                            &mut self.patches,
                            Patch {
                                base: header.base_reference(),
                                source: reference,
                                contribution,
                            },
                        )?,
                        Err(error) => decode::contribution_error(error, &mut self.stats)?,
                    }
                }
            }
            _ => return Err(stream_error("unsupported parser classification")),
        }
        let root_valid = slot.classification == RecordKind::Base
            && slot.base.is_some()
            && slot
                .flags
                .is_some_and(|flags| flags.contains(RecordFlags::DIRECTORY));
        push(&mut self.slots, slot)?;
        if ordinal == ROOT && !root_valid {
            return Err(ScanError::RequiredRecord { record: ROOT });
        }
        Ok(())
    }
}

fn set_header(slot: &mut RecordSlot, header: RecordHeader) {
    slot.sequence = Some(header.sequence());
    slot.flags = Some(header.flags());
}

fn record_error(error: RecordError, stats: &mut ScanStats) -> Result<(), ScanError> {
    match error {
        RecordError::Fixup {
            source: FixupError::TrailerMismatch { .. },
        } => bump(&mut stats.bad_fixup),
        RecordError::Fixup { .. } => bump(&mut stats.invalid_fixup_metadata),
        RecordError::InvalidSignature { .. } => bump(&mut stats.invalid_signature),
        _ => bump(&mut stats.corrupt_headers),
    }
}
