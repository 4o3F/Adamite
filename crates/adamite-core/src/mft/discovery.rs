//! Finite attribute-list acquisition and reachable MFT extension discovery.

use super::{
    bootstrap::{AttributeListDescriptor, ListStorage, Record0, RecordParts},
    mapping::{MftMapping, ReadPiece, RecordAssembly},
};
use crate::{
    error::ScanError,
    util::{SliceExt, VecExt},
};
use adamite_ntfs::{
    attribute::{
        AttributeType,
        runlist::{Runlist, VcnRange},
    },
    record::reference::FileRef,
    value::attribute_list::AttributeList,
};

/// NTFS parser's complete ATTRIBUTE_LIST value bound, in bytes.
const MAX_LIST_BYTES: u64 = 262_144;
/// Maximum newly decoded list entries in one CPU step.
const ENTRIES_PER_STEP: usize = 256;

#[derive(Debug, PartialEq, Eq)]
struct SegmentRef {
    source: FileRef,
    attribute_type: AttributeType,
    name: Vec<u16>,
    id: u16,
    lowest: u64,
}

#[derive(Debug)]
struct Pending {
    key: SegmentRef,
    done: bool,
}

#[derive(Debug)]
enum ListValue {
    Resident(Option<Vec<u8>>),
    NonResident {
        mapping: MftMapping,
        len: Option<u64>,
    },
}

#[derive(Debug)]
struct ListStream {
    name: Vec<u16>,
    value: ListValue,
    parsed: bool,
}

#[derive(Debug)]
enum ReadTarget {
    List(usize),
    Extension(usize),
}

#[derive(Debug)]
struct ActiveRead {
    target: ReadTarget,
    assembly: RecordAssembly,
}

#[derive(Debug)]
struct Parsing {
    list: usize,
    bytes: Vec<u8>,
    cursor: usize,
}

#[derive(Debug)]
pub(crate) enum DiscoveryStep {
    Read,
    Progress,
    Complete,
}

#[derive(Debug)]
pub(crate) struct Discovery {
    pending: Vec<Pending>,
    records: Vec<RecordParts>,
    committed: Vec<(FileRef, u16, u64)>,
    descriptors: Vec<(FileRef, AttributeListDescriptor)>,
    lists: Vec<ListStream>,
    active: Option<ActiveRead>,
    parsing: Option<Parsing>,
    pending_cursor: usize,
    unresolved: bool,
}

impl Discovery {
    pub(crate) fn new(base: &mut Record0) -> Result<Self, ScanError> {
        let mut state = Self {
            pending: Vec::new(),
            records: Vec::new(),
            committed: Vec::new(),
            descriptors: Vec::new(),
            lists: Vec::new(),
            active: None,
            parsing: None,
            pending_cursor: 0,
            unresolved: false,
        };
        let segments = std::mem::take(&mut base.segments);
        for segment in &segments {
            if segment.lowest_vcn != 0 {
                base.mapping = base.mapping.with_runs(&segment.runs)?;
            }
            state
                .committed
                .try_push((base.reference, segment.id, segment.lowest_vcn))?;
        }
        state.add_lists(
            base.reference,
            std::mem::take(&mut base.attribute_lists),
            &base.mapping,
        )?;
        state.records.try_push(RecordParts {
            reference: base.reference,
            segments,
            lists: Vec::new(),
        })?;
        Ok(state)
    }

    fn add_lists(
        &mut self,
        source: FileRef,
        descriptors: Vec<AttributeListDescriptor>,
        mft: &MftMapping,
    ) -> Result<(), ScanError> {
        for descriptor in descriptors {
            if let Some((_, seen)) = self
                .descriptors
                .iter()
                .find(|(origin, seen)| *origin == source && seen.id == descriptor.id)
            {
                if seen == &descriptor {
                    continue;
                }
                return Err(ScanError::bootstrap(
                    "conflicting attribute-list descriptor identity",
                ));
            }
            descriptor.validate_storage()?;
            let value = match &descriptor.storage {
                ListStorage::Resident(bytes) => {
                    // Check the value bound here; entries are parsed in bounded steps later.
                    let _ = AttributeList::new(bytes)
                        .map_err(|source| ScanError::AttributeList { source })?;
                    ListValue::Resident(Some(bytes.try_copy()?))
                }
                ListStorage::NonResident {
                    lowest_vcn,
                    highest_vcn,
                    compression_unit: _,
                    sizes,
                    mapping_pairs,
                } => {
                    let lowest = i64::try_from(*lowest_vcn)
                        .map_err(|_| ScanError::overflow("attribute-list lowest VCN"))?;
                    let range = VcnRange::new(lowest, *highest_vcn)
                        .map_err(|source| ScanError::Runlist { source })?;
                    let mapping =
                        MftMapping::from_runlist(Runlist::new(mapping_pairs, range), mft.geometry)?;
                    let len = sizes.map(|sizes| sizes.data());
                    if let Some(size) = len.filter(|size| *size > MAX_LIST_BYTES) {
                        return Err(ScanError::AttributeListTooLarge { size });
                    }
                    ListValue::NonResident { mapping, len }
                }
            };
            if let Some(stream) = self
                .lists
                .iter_mut()
                .find(|list| list.name == descriptor.name)
            {
                match (&mut stream.value, value) {
                    (
                        ListValue::NonResident { mapping, len },
                        ListValue::NonResident {
                            mapping: added,
                            len: added_len,
                        },
                    ) => {
                        if len.is_some() && added_len.is_some() {
                            return Err(ScanError::bootstrap(
                                "duplicate attribute-list first segment",
                            ));
                        }
                        let merged = mapping.with_runs(&added.runs)?;
                        *mapping = merged;
                        if added_len.is_some() {
                            *len = added_len;
                        }
                    }
                    _ => {
                        return Err(ScanError::bootstrap(
                            "conflicting resident attribute-list streams",
                        ));
                    }
                }
            } else {
                self.lists.try_push(ListStream {
                    name: descriptor.name.try_copy()?,
                    value,
                    parsed: false,
                })?;
            }
            self.descriptors.try_push((source, descriptor))?;
        }
        Ok(())
    }

    fn add_reference(&mut self, key: SegmentRef) -> Result<(), ScanError> {
        if self.pending.iter().any(|pending| pending.key == key) {
            return Ok(());
        }
        if self.pending.iter().any(|pending| {
            pending.key.source.record() == key.source.record()
                && pending.key.attribute_type == key.attribute_type
                && pending.key.name == key.name
                && pending.key.id == key.id
        }) {
            return Err(ScanError::bootstrap(
                "conflicting attribute-list segment reference",
            ));
        }
        self.pending.try_push(Pending { key, done: false })?;
        Ok(())
    }

    fn parse_list(&mut self) -> Result<(), ScanError> {
        let Some(mut parsing) = self.parsing.take() else {
            return Ok(());
        };
        for _ in 0..ENTRIES_PER_STEP {
            let suffix = parsing
                .bytes
                .get(parsing.cursor..)
                .ok_or_else(|| ScanError::overflow("list parser cursor"))?;
            let mut entries =
                AttributeList::new(suffix).map_err(|source| ScanError::AttributeList { source })?;
            let Some(entry) = entries.next() else {
                self.lists
                    .get_mut(parsing.list)
                    .ok_or_else(|| ScanError::overflow("list index"))?
                    .parsed = true;
                return Ok(());
            };
            let entry = entry.map_err(|source| ScanError::AttributeList { source })?;
            if (entry.attribute_type() == AttributeType::DATA && entry.name().len_units() == 0)
                || entry.attribute_type() == AttributeType::ATTRIBUTE_LIST
            {
                let mut name = Vec::new();
                name.try_reserve_exact(entry.name().len_units())?;
                name.extend(entry.name().units());
                self.add_reference(SegmentRef {
                    source: entry.segment_reference(),
                    attribute_type: entry.attribute_type(),
                    name,
                    id: entry.attribute_id(),
                    lowest: entry.lowest_vcn(),
                })?;
            }
            // The parser validated this entry length; retain only its byte cursor across calls.
            let length = suffix
                .get(4..6)
                .and_then(|bytes| <[u8; 2]>::try_from(bytes).ok())
                .map(u16::from_le_bytes)
                .ok_or_else(|| ScanError::overflow("list entry length"))?;
            parsing.cursor = parsing
                .cursor
                .checked_add(usize::from(length))
                .ok_or_else(|| ScanError::overflow("list parser cursor"))?;
        }
        self.parsing = Some(parsing);
        Ok(())
    }

    fn has_target(&self, record: &RecordParts, key: &SegmentRef) -> bool {
        if key.attribute_type == AttributeType::DATA {
            key.name.is_empty()
                && record
                    .segments
                    .iter()
                    .any(|segment| segment.id == key.id && segment.lowest_vcn == key.lowest)
        } else {
            record.lists.iter().any(|descriptor| {
                descriptor.id == key.id
                    && descriptor.name == key.name
                    && descriptor.lowest_vcn() == key.lowest
            }) || self.descriptors.iter().any(|(origin, descriptor)| {
                *origin == key.source
                    && descriptor.id == key.id
                    && descriptor.name == key.name
                    && descriptor.lowest_vcn() == key.lowest
            })
        }
    }

    fn commit_cached(&mut self, index: usize, base: &mut Record0) -> Result<bool, ScanError> {
        let pending = self
            .pending
            .get(index)
            .ok_or_else(|| ScanError::overflow("pending reference index"))?;
        let key = &pending.key;
        let Some(record) = self
            .records
            .iter()
            .find(|record| record.reference.record() == key.source.record())
        else {
            return Ok(false);
        };
        if record.reference != key.source {
            return Err(ScanError::bootstrap("stale cached extension sequence"));
        }
        if !self.has_target(record, key) {
            return Err(ScanError::bootstrap(
                "missing referenced bootstrap attribute segment",
            ));
        }
        if key.attribute_type == AttributeType::DATA
            && !self.committed.contains(&(key.source, key.id, key.lowest))
        {
            let segment = record
                .segments
                .iter()
                .find(|segment| segment.id == key.id && segment.lowest_vcn == key.lowest)
                .ok_or_else(|| ScanError::bootstrap("missing MFT DATA segment"))?;
            let mapping = base.mapping.with_runs(&segment.runs)?;
            self.committed.try_push((key.source, key.id, key.lowest))?;
            base.mapping = mapping;
        }
        self.pending
            .get_mut(index)
            .ok_or_else(|| ScanError::overflow("pending reference index"))?
            .done = true;
        Ok(true)
    }

    fn read_step(&mut self, base: &mut Record0) -> Result<DiscoveryStep, ScanError> {
        let active = self
            .active
            .as_mut()
            .ok_or_else(|| ScanError::bootstrap("missing discovery read"))?;
        if active.assembly.complete() {
            let active = self
                .active
                .take()
                .ok_or_else(|| ScanError::bootstrap("missing completed discovery read"))?;
            let bytes = active.assembly.into_bytes();
            match active.target {
                ReadTarget::List(list) => {
                    self.parsing = Some(Parsing {
                        list,
                        bytes,
                        cursor: 0,
                    })
                }
                ReadTarget::Extension(index) => {
                    let key = &self
                        .pending
                        .get(index)
                        .ok_or_else(|| ScanError::overflow("pending reference index"))?
                        .key;
                    let mut record = RecordParts::parse_extension(
                        bytes,
                        key.source,
                        base.reference,
                        base.mapping.geometry,
                    )?;
                    if !self.has_target(&record, key) {
                        return Err(ScanError::bootstrap(
                            "missing referenced bootstrap attribute segment",
                        ));
                    }
                    self.add_lists(
                        record.reference,
                        std::mem::take(&mut record.lists),
                        &base.mapping,
                    )?;
                    self.records.try_push(record)?;
                    self.commit_cached(index, base)?;
                }
            }
            return Ok(DiscoveryStep::Progress);
        }
        active.mapping(&self.lists, base)?;
        Ok(DiscoveryStep::Read)
    }

    pub(crate) fn is_reading(&self) -> bool {
        self.active.is_some()
    }

    pub(crate) fn accept_bytes(&mut self, bytes: &[u8]) -> Result<(), ScanError> {
        self.active
            .as_mut()
            .ok_or_else(|| ScanError::bootstrap("missing discovery read"))?
            .assembly
            .append(bytes)
    }

    /// Plans without advancing the assembly; the driver commits after issuing the read.
    pub(crate) fn next_piece(
        &self,
        base: &Record0,
        chunk: usize,
    ) -> Result<Option<ReadPiece>, ScanError> {
        let active = self
            .active
            .as_ref()
            .ok_or_else(|| ScanError::bootstrap("missing discovery read"))?;
        active
            .assembly
            .next_piece(active.mapping(&self.lists, base)?, chunk)
    }

    pub(crate) fn planned(&mut self, len: usize) -> Result<(), ScanError> {
        self.active
            .as_mut()
            .ok_or_else(|| ScanError::bootstrap("missing discovery read"))?
            .assembly
            .planned(len)
    }

    pub(crate) fn step(&mut self, base: &mut Record0) -> Result<DiscoveryStep, ScanError> {
        if self.active.is_some() {
            return self.read_step(base);
        }
        if self.parsing.is_some() {
            self.parse_list()?;
            return Ok(DiscoveryStep::Progress);
        }
        for (index, stream) in self.lists.iter_mut().enumerate() {
            if stream.parsed {
                continue;
            }
            match &mut stream.value {
                ListValue::Resident(value) => {
                    self.parsing = Some(Parsing {
                        list: index,
                        bytes: value.take().ok_or_else(|| {
                            ScanError::bootstrap("resident list already consumed")
                        })?,
                        cursor: 0,
                    });
                    return Ok(DiscoveryStep::Progress);
                }
                ListValue::NonResident {
                    mapping,
                    len: Some(len),
                } if mapping.covers(0, *len)? => {
                    self.active = Some(ActiveRead {
                        target: ReadTarget::List(index),
                        assembly: RecordAssembly::new(
                            0,
                            usize::try_from(*len)
                                .map_err(|_| ScanError::overflow("attribute-list length"))?,
                        )?,
                    });
                    return Ok(DiscoveryStep::Progress);
                }
                _ => {}
            }
        }
        let end = self
            .pending_cursor
            .checked_add(ENTRIES_PER_STEP)
            .ok_or_else(|| ScanError::overflow("pending reference batch"))?
            .min(self.pending.len());
        while self.pending_cursor < end {
            let index = self.pending_cursor;
            self.pending_cursor = self
                .pending_cursor
                .checked_add(1)
                .ok_or_else(|| ScanError::overflow("pending reference cursor"))?;
            let pending = self
                .pending
                .get(index)
                .ok_or_else(|| ScanError::overflow("pending reference index"))?;
            if pending.done {
                continue;
            }
            let start = pending
                .key
                .source
                .record()
                .checked_mul(u64::from(base.mapping.geometry.file_record_size()))
                .ok_or_else(|| ScanError::overflow("extension byte offset"))?;
            let len = u64::from(base.mapping.geometry.file_record_size());
            if start
                .checked_add(len)
                .ok_or_else(|| ScanError::overflow("extension byte end"))?
                > base.data_size
            {
                return Err(ScanError::bootstrap(
                    "extension record exceeds logical MFT size",
                ));
            }
            if self.commit_cached(index, base)? {
                self.pending_cursor = 0;
                self.unresolved = false;
                return Ok(DiscoveryStep::Progress);
            }
            if base.mapping.covers(start, len)? {
                self.active = Some(ActiveRead {
                    target: ReadTarget::Extension(index),
                    assembly: RecordAssembly::new(
                        start,
                        usize::try_from(len)
                            .map_err(|_| ScanError::overflow("extension record length"))?,
                    )?,
                });
                self.pending_cursor = 0;
                self.unresolved = false;
                return Ok(DiscoveryStep::Progress);
            }
            self.unresolved = true;
        }
        if self.pending_cursor < self.pending.len() {
            return Ok(DiscoveryStep::Progress);
        }
        // Every eligible list and reference was considered, with no I/O or CPU work left.
        if self.lists.iter().all(|list| list.parsed)
            && !self.unresolved
            && base.mapping.covers(0, base.data_size)?
        {
            return Ok(DiscoveryStep::Complete);
        }
        Err(ScanError::BootstrapIncomplete)
    }
}

impl ActiveRead {
    fn mapping<'a>(
        &self,
        lists: &'a [ListStream],
        base: &'a Record0,
    ) -> Result<&'a MftMapping, ScanError> {
        match self.target {
            ReadTarget::Extension(_) => Ok(&base.mapping),
            ReadTarget::List(index) => match &lists
                .get(index)
                .ok_or_else(|| ScanError::overflow("list index"))?
                .value
            {
                ListValue::NonResident { mapping, .. } => Ok(mapping),
                _ => Err(ScanError::bootstrap("resident list scheduled for I/O")),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mft::mapping::OwnedRun;
    use adamite_fixtures::builders::BootSectorBuilder;
    use std::error::Error;

    fn base() -> Result<Record0, Box<dyn Error>> {
        let geometry = adamite_ntfs::boot::VolumeGeometry::from_boot_sector(
            &BootSectorBuilder {
                cluster_encoding: 1,
                sectors: 4096,
                ..BootSectorBuilder::default()
            }
            .build(),
        )?;
        Ok(Record0 {
            reference: FileRef::new(0, 7)?,
            data_id: 0,
            data_size: 1_048_576,
            mapping: MftMapping::new(
                geometry,
                vec![OwnedRun {
                    vcn: 0,
                    lcn: 4,
                    clusters: 2,
                }],
            )?,
            attribute_lists: vec![],
            segments: vec![],
        })
    }

    #[test]
    fn bootstrap_pending_references_yield_before_no_progress_failure() -> Result<(), Box<dyn Error>>
    {
        let mut base = base()?;
        let mut discovery = Discovery::new(&mut base)?;
        for record in 10..310 {
            discovery.add_reference(SegmentRef {
                source: FileRef::new(record, 7)?,
                attribute_type: AttributeType::DATA,
                name: vec![],
                id: 0,
                lowest: 0,
            })?;
        }
        assert!(
            matches!(discovery.step(&mut base)?, DiscoveryStep::Progress),
            "a partial reference pass must yield"
        );
        assert_eq!(discovery.pending_cursor, 256);
        assert!(
            matches!(
                discovery.step(&mut base),
                Err(ScanError::BootstrapIncomplete)
            ),
            "a completed pass must detect no progress"
        );
        assert_eq!(discovery.pending_cursor, 300);
        Ok(())
    }

    #[test]
    fn bootstrap_discovery_planning_waits_for_commit_before_accepting_bytes()
    -> Result<(), Box<dyn Error>> {
        let mut base = base()?;
        let mut discovery = Discovery::new(&mut base)?;
        discovery.active = Some(ActiveRead {
            target: ReadTarget::Extension(0),
            assembly: RecordAssembly::new(0, 1024)?,
        });
        assert!(discovery.is_reading());
        assert!(matches!(discovery.step(&mut base)?, DiscoveryStep::Read));
        let first = discovery.next_piece(&base, 512)?.ok_or("first piece")?;
        assert_eq!(
            first,
            ReadPiece {
                offset: 2048,
                len: 512
            }
        );
        assert_eq!(discovery.next_piece(&base, 512)?, Some(first));
        let bytes = [0; 512];
        assert!(matches!(
            discovery.accept_bytes(&bytes),
            Err(ScanError::Bootstrap {
                reason: "response exceeds planned record bytes"
            })
        ));
        assert_eq!(discovery.next_piece(&base, 512)?, Some(first));
        discovery.planned(first.len)?;
        discovery.accept_bytes(&bytes)?;
        assert!(matches!(discovery.step(&mut base)?, DiscoveryStep::Read));
        let second = discovery.next_piece(&base, 512)?.ok_or("second piece")?;
        assert_eq!(
            second,
            ReadPiece {
                offset: 2560,
                len: 512
            }
        );
        discovery.planned(second.len)?;
        assert_eq!(discovery.next_piece(&base, 512)?, None);
        discovery.accept_bytes(&bytes)?;
        assert!(
            discovery
                .active
                .as_ref()
                .ok_or("active read")?
                .assembly
                .complete()
        );
        Ok(())
    }
}
