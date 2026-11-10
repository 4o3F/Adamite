//! Bounded MFT byte assembly, ordinal classification and owned record contributions.

mod decode;

use crate::{
    ScanStats,
    error::ScanError,
    mft::{
        mapping::MftMapping,
        model::{Patch, RecordContribution, RecordKind, RecordSlot},
    },
    util::{CounterExt, VecExt},
};
use adamite_ntfs::record::{
    RecordClass, RecordError, RecordFlags, RecordHeader, fixup::FixupError, reference::FileRef,
    system::ROOT,
};

/// A checked read range and the virtual cursor to commit after successful issue.
#[derive(Debug)]
pub(crate) struct ReadPlan {
    pub offset: u64,
    pub len: usize,
    end: u64,
}

#[derive(Debug)]
pub(crate) struct Ingest {
    data_size: u64,
    record_size: usize,
    planned: u64,
    raw: Vec<u8>,
    pub slots: Vec<RecordSlot>,
    pub patches: Vec<Patch>,
    pub stats: ScanStats,
}

impl Ingest {
    pub(crate) fn new(data_size: u64, record_size: u32) -> Result<Self, ScanError> {
        let record_size =
            usize::try_from(record_size).map_err(|_| ScanError::overflow("record size"))?;
        let mut raw = Vec::new();
        raw.try_reserve_exact(record_size)?;
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

    pub(crate) fn buffer_capacity(&self) -> usize {
        self.raw.capacity()
    }

    pub(crate) fn record_size(&self) -> usize {
        self.record_size
    }

    pub(crate) fn has_partial_record(&self) -> bool {
        !self.raw.is_empty()
    }

    pub(crate) fn has_unplanned(&self) -> bool {
        self.planned < self.data_size
    }

    pub(crate) fn next_piece(
        &self,
        mapping: &MftMapping,
        chunk: usize,
    ) -> Result<Option<ReadPlan>, ScanError> {
        if !self.has_unplanned() {
            return Ok(None);
        }
        let remaining = self
            .data_size
            .checked_sub(self.planned)
            .ok_or_else(|| ScanError::overflow("unplanned MFT bytes"))?;
        let len = usize::try_from(
            remaining.min(u64::try_from(chunk).map_err(|_| ScanError::overflow("chunk length"))?),
        )
        .map_err(|_| ScanError::overflow("stream request length"))?;
        let piece = mapping.read_piece(self.planned, len)?;
        let end = self
            .planned
            .checked_add(
                u64::try_from(piece.len)
                    .map_err(|_| ScanError::overflow("stream request length"))?,
            )
            .ok_or_else(|| ScanError::overflow("stream planning cursor"))?;
        Ok(Some(ReadPlan {
            offset: piece.offset,
            len: piece.len,
            end,
        }))
    }

    /// Commits the prechecked cursor after the driver successfully issues this range.
    pub(crate) fn planned(&mut self, plan: ReadPlan) {
        self.planned = plan.end;
    }

    /// Appends at most the prefix needed to finish one record.
    pub(crate) fn accept_prefix(&mut self, bytes: &[u8]) -> Result<usize, ScanError> {
        let remaining = self
            .record_size
            .checked_sub(self.raw.len())
            .ok_or_else(|| ScanError::overflow("partial record remainder"))?;
        let len = remaining.min(bytes.len());
        self.raw.extend_from_slice(
            bytes
                .get(..len)
                .ok_or_else(|| ScanError::stream("missing response prefix"))?,
        );
        Ok(len)
    }

    /// Decodes a complete record after its response prefix has been consumed.
    pub(crate) fn finish_record(&mut self, mapping: &MftMapping) -> Result<bool, ScanError> {
        if self.raw.len() != self.record_size {
            return Ok(false);
        }
        self.record(mapping)?;
        self.raw.clear();
        Ok(true)
    }

    pub(crate) fn is_complete(&self) -> Result<bool, ScanError> {
        let consumed = self
            .stats
            .records_total
            .checked_mul(
                u64::try_from(self.record_size).map_err(|_| ScanError::overflow("record size"))?,
            )
            .ok_or_else(|| ScanError::overflow("consumed MFT bytes"))?;
        if consumed == self.data_size {
            if self
                .slots
                .get(usize::try_from(ROOT).map_err(|_| ScanError::overflow("root slot index"))?)
                .is_none()
            {
                return Err(ScanError::RequiredRecord { record: ROOT });
            }
            return Ok(true);
        }
        Ok(false)
    }

    fn record(&mut self, mapping: &MftMapping) -> Result<(), ScanError> {
        let ordinal = self.stats.records_total;
        self.stats.records_total.try_bump()?;
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
                self.stats.unused.try_bump()?;
            }
            RecordClass::Baad => {
                slot.classification = RecordKind::Baad;
                self.stats.baad.try_bump()?;
            }
            RecordClass::NotInUse(header) => {
                slot.classification = RecordKind::Deleted;
                slot.set_header(header);
                self.stats.deleted.try_bump()?;
                // Unallocated FILE slots can leave the stored record number unset.
                if header
                    .record_number()
                    .is_some_and(|number| number != 0 && u64::from(number) != ordinal)
                {
                    self.stats.corrupt_records.try_bump()?;
                    self.stats.corrupt_headers.try_bump()?;
                }
            }
            RecordClass::Corrupt(error) => {
                self.stats.corrupt_records.try_bump()?;
                Self::record_error(error, &mut self.stats)?;
            }
            RecordClass::Base(record) | RecordClass::Extension(record) => {
                let header = record.header();
                let base = header.base_reference().is_null();
                slot.classification = if base {
                    RecordKind::Base
                } else {
                    RecordKind::Extension
                };
                slot.set_header(header);
                self.stats.records_in_use.try_bump()?;
                let counter = if base {
                    &mut self.stats.base_records
                } else {
                    &mut self.stats.extension_records
                };
                counter.try_bump()?;
                if header
                    .record_number()
                    .is_some_and(|number| u64::from(number) != ordinal)
                {
                    self.stats.corrupt_records.try_bump()?;
                    self.stats.corrupt_headers.try_bump()?;
                } else {
                    let reference = match FileRef::new(ordinal, header.sequence()) {
                        Ok(reference) => reference,
                        Err(source) => {
                            self.stats.invalid_file_refs.try_bump()?;
                            return Err(ScanError::FileRef { source });
                        }
                    };
                    match RecordContribution::decode(&record, reference, mapping.geometry) {
                        Ok(contribution) if base => slot.base = Some(contribution),
                        Ok(contribution) => self.patches.try_push(Patch {
                            base: header.base_reference(),
                            source: reference,
                            contribution,
                        })?,
                        Err(error) => error.account(&mut self.stats)?,
                    }
                }
            }
            _ => return Err(ScanError::stream("unsupported parser classification")),
        }
        let root_valid = slot.classification == RecordKind::Base
            && slot.base.is_some()
            && slot
                .flags
                .is_some_and(|flags| flags.contains(RecordFlags::DIRECTORY));
        self.slots.try_push(slot)?;
        if ordinal == ROOT && !root_valid {
            return Err(ScanError::RequiredRecord { record: ROOT });
        }
        Ok(())
    }

    fn record_error(error: RecordError, stats: &mut ScanStats) -> Result<(), ScanError> {
        let counter = match error {
            RecordError::Fixup {
                source: FixupError::TrailerMismatch { .. },
            } => &mut stats.bad_fixup,
            RecordError::Fixup { .. } => &mut stats.invalid_fixup_metadata,
            RecordError::InvalidSignature { .. } => &mut stats.invalid_signature,
            _ => &mut stats.corrupt_headers,
        };
        counter.try_bump()?;
        Ok(())
    }
}

impl RecordSlot {
    fn set_header(&mut self, header: RecordHeader) {
        self.sequence = Some(header.sequence());
        self.flags = Some(header.flags());
    }
}
