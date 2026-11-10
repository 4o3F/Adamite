//! Finite attribute-list acquisition and reachable MFT extension discovery.

use super::{
    ReadRequest, ScanConfig,
    bootstrap::{
        self, AttributeListDescriptor, ListStorage, Record0, RecordParts, check_storage,
        copy_slice, push, unsupported,
    },
    mapping::{MftMapping, RecordAssembly, overflow},
    protocol::Requests,
    scanner::ScanError,
};
use adamite_ntfs::{AttributeList, AttributeType, FileRef, Runlist, VcnRange};

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
pub(super) enum DiscoveryStep {
    Need(ReadRequest),
    Progress { waiting: bool },
    Complete,
}

#[derive(Debug)]
pub(super) struct Discovery {
    pending: Vec<Pending>,
    records: Vec<RecordParts>,
    committed: Vec<(FileRef, u16, u64)>,
    descriptors: Vec<(FileRef, AttributeListDescriptor)>,
    lists: Vec<ListStream>,
    active: Option<ActiveRead>,
    parsing: Option<Parsing>,
}

impl Discovery {
    pub(super) fn new(base: &mut Record0) -> Result<Self, ScanError> {
        let mut state = Self {
            pending: Vec::new(),
            records: Vec::new(),
            committed: Vec::new(),
            descriptors: Vec::new(),
            lists: Vec::new(),
            active: None,
            parsing: None,
        };
        let segments = std::mem::take(&mut base.segments);
        for segment in &segments {
            if segment.lowest_vcn != 0 {
                base.mapping = base.mapping.with_runs(&segment.runs)?;
            }
            push(
                &mut state.committed,
                (base.reference, segment.id, segment.lowest_vcn),
            )?;
        }
        state.add_lists(
            base.reference,
            std::mem::take(&mut base.attribute_lists),
            &base.mapping,
        )?;
        push(
            &mut state.records,
            RecordParts {
                reference: base.reference,
                segments,
                lists: Vec::new(),
            },
        )?;
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
                return Err(unsupported(
                    "conflicting attribute-list descriptor identity",
                ));
            }
            let value = match &descriptor.storage {
                ListStorage::Resident(bytes) => {
                    check_storage(descriptor.flags, 0)?;
                    // Check the value bound here; entries are parsed in bounded steps later.
                    let _ = AttributeList::new(bytes)
                        .map_err(|source| ScanError::AttributeList { source })?;
                    ListValue::Resident(Some(copy_slice(bytes)?))
                }
                ListStorage::NonResident {
                    lowest_vcn,
                    highest_vcn,
                    compression_unit,
                    sizes,
                    mapping_pairs,
                } => {
                    check_storage(descriptor.flags, *compression_unit)?;
                    let lowest = i64::try_from(*lowest_vcn)
                        .map_err(|_| overflow("attribute-list lowest VCN"))?;
                    let range = VcnRange::new(lowest, *highest_vcn)
                        .map_err(|source| ScanError::Runlist { source })?;
                    let mapping = bootstrap::collect_mapping(
                        Runlist::new(mapping_pairs, range),
                        mft.geometry,
                    )?;
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
                            return Err(unsupported("duplicate attribute-list first segment"));
                        }
                        let merged = mapping.with_runs(&added.runs)?;
                        *mapping = merged;
                        if added_len.is_some() {
                            *len = added_len;
                        }
                    }
                    _ => return Err(unsupported("conflicting resident attribute-list streams")),
                }
            } else {
                push(
                    &mut self.lists,
                    ListStream {
                        name: copy_slice(&descriptor.name)?,
                        value,
                        parsed: false,
                    },
                )?;
            }
            push(&mut self.descriptors, (source, descriptor))?;
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
            return Err(unsupported("conflicting attribute-list segment reference"));
        }
        push(&mut self.pending, Pending { key, done: false })
    }

    fn parse_list(&mut self) -> Result<(), ScanError> {
        let Some(mut parsing) = self.parsing.take() else {
            return Ok(());
        };
        for _ in 0..ENTRIES_PER_STEP {
            let suffix = parsing
                .bytes
                .get(parsing.cursor..)
                .ok_or_else(|| overflow("list parser cursor"))?;
            let mut entries =
                AttributeList::new(suffix).map_err(|source| ScanError::AttributeList { source })?;
            let Some(entry) = entries.next() else {
                self.lists
                    .get_mut(parsing.list)
                    .ok_or_else(|| overflow("list index"))?
                    .parsed = true;
                return Ok(());
            };
            let entry = entry.map_err(|source| ScanError::AttributeList { source })?;
            if (entry.attribute_type() == AttributeType::DATA && entry.name().len_units() == 0)
                || entry.attribute_type() == AttributeType::ATTRIBUTE_LIST
            {
                let mut name = Vec::new();
                name.try_reserve_exact(entry.name().len_units())
                    .map_err(|source| ScanError::Allocation { source })?;
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
                .ok_or_else(|| overflow("list entry length"))?;
            parsing.cursor = parsing
                .cursor
                .checked_add(usize::from(length))
                .ok_or_else(|| overflow("list parser cursor"))?;
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
                    && list_lowest(descriptor) == key.lowest
            }) || self.descriptors.iter().any(|(origin, descriptor)| {
                *origin == key.source
                    && descriptor.id == key.id
                    && descriptor.name == key.name
                    && list_lowest(descriptor) == key.lowest
            })
        }
    }

    fn commit_cached(&mut self, index: usize, base: &mut Record0) -> Result<bool, ScanError> {
        let pending = self
            .pending
            .get(index)
            .ok_or_else(|| overflow("pending reference index"))?;
        let key = &pending.key;
        let Some(record) = self
            .records
            .iter()
            .find(|record| record.reference.record() == key.source.record())
        else {
            return Ok(false);
        };
        if record.reference != key.source {
            return Err(unsupported("stale cached extension sequence"));
        }
        if !self.has_target(record, key) {
            return Err(unsupported(
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
                .ok_or_else(|| unsupported("missing MFT DATA segment"))?;
            let mapping = base.mapping.with_runs(&segment.runs)?;
            push(&mut self.committed, (key.source, key.id, key.lowest))?;
            base.mapping = mapping;
        }
        self.pending
            .get_mut(index)
            .ok_or_else(|| overflow("pending reference index"))?
            .done = true;
        Ok(true)
    }

    fn read_step(
        &mut self,
        base: &mut Record0,
        requests: &mut Requests,
        config: &ScanConfig,
    ) -> Result<DiscoveryStep, ScanError> {
        let active = self
            .active
            .as_mut()
            .ok_or_else(|| unsupported("missing discovery read"))?;
        if let Some((_, bytes)) = requests.take_ready() {
            active.assembly.append(&bytes)?;
        }
        if active.assembly.complete() {
            let active = self
                .active
                .take()
                .ok_or_else(|| unsupported("missing completed discovery read"))?;
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
                        .ok_or_else(|| overflow("pending reference index"))?
                        .key;
                    let mut record = bootstrap::parse_extension(
                        bytes,
                        key.source,
                        base.reference,
                        base.mapping.geometry,
                    )?;
                    if !self.has_target(&record, key) {
                        return Err(unsupported(
                            "missing referenced bootstrap attribute segment",
                        ));
                    }
                    self.add_lists(
                        record.reference,
                        std::mem::take(&mut record.lists),
                        &base.mapping,
                    )?;
                    push(&mut self.records, record)?;
                    self.commit_cached(index, base)?;
                }
            }
            return Ok(DiscoveryStep::Progress { waiting: false });
        }
        let mapping = match active.target {
            ReadTarget::Extension(_) => &base.mapping,
            ReadTarget::List(index) => match &self
                .lists
                .get(index)
                .ok_or_else(|| overflow("list index"))?
                .value
            {
                ListValue::NonResident { mapping, .. } => mapping,
                _ => return Err(unsupported("resident list scheduled for I/O")),
            },
        };
        if requests.len() < config.max_outstanding
            && let Some(piece) = active.assembly.next_piece(mapping, config.chunk_size)?
        {
            let request = requests.issue(piece.offset, piece.len)?;
            active.assembly.planned(piece.len)?;
            return Ok(DiscoveryStep::Need(request));
        }
        Ok(DiscoveryStep::Progress {
            waiting: !requests.has_ready(),
        })
    }

    pub(super) fn step(
        &mut self,
        base: &mut Record0,
        requests: &mut Requests,
        config: &ScanConfig,
    ) -> Result<DiscoveryStep, ScanError> {
        if self.active.is_some() {
            return self.read_step(base, requests, config);
        }
        if self.parsing.is_some() {
            self.parse_list()?;
            return Ok(DiscoveryStep::Progress { waiting: false });
        }
        for (index, stream) in self.lists.iter_mut().enumerate() {
            if stream.parsed {
                continue;
            }
            match &mut stream.value {
                ListValue::Resident(value) => {
                    self.parsing = Some(Parsing {
                        list: index,
                        bytes: value
                            .take()
                            .ok_or_else(|| unsupported("resident list already consumed"))?,
                        cursor: 0,
                    });
                    return Ok(DiscoveryStep::Progress { waiting: false });
                }
                ListValue::NonResident {
                    mapping,
                    len: Some(len),
                } if mapping.covers(0, *len)? => {
                    self.active = Some(ActiveRead {
                        target: ReadTarget::List(index),
                        assembly: RecordAssembly::new(
                            0,
                            usize::try_from(*len).map_err(|_| overflow("attribute-list length"))?,
                        )?,
                    });
                    return Ok(DiscoveryStep::Progress { waiting: false });
                }
                _ => {}
            }
        }
        for index in 0..self.pending.len() {
            let pending = self
                .pending
                .get(index)
                .ok_or_else(|| overflow("pending reference index"))?;
            if pending.done {
                continue;
            }
            let start = pending
                .key
                .source
                .record()
                .checked_mul(u64::from(base.mapping.geometry.file_record_size()))
                .ok_or_else(|| overflow("extension byte offset"))?;
            let len = u64::from(base.mapping.geometry.file_record_size());
            if start
                .checked_add(len)
                .ok_or_else(|| overflow("extension byte end"))?
                > base.data_size
            {
                return Err(unsupported("extension record exceeds logical MFT size"));
            }
            if self.commit_cached(index, base)? {
                return Ok(DiscoveryStep::Progress { waiting: false });
            }
            if base.mapping.covers(start, len)? {
                self.active = Some(ActiveRead {
                    target: ReadTarget::Extension(index),
                    assembly: RecordAssembly::new(
                        start,
                        usize::try_from(len).map_err(|_| overflow("extension record length"))?,
                    )?,
                });
                return Ok(DiscoveryStep::Progress { waiting: false });
            }
        }
        // Every eligible list and reference was considered, with no I/O or CPU work left.
        if self.lists.iter().all(|list| list.parsed)
            && self.pending.iter().all(|pending| pending.done)
            && base.mapping.covers(0, base.data_size)?
        {
            return Ok(DiscoveryStep::Complete);
        }
        Err(ScanError::BootstrapIncomplete)
    }
}

fn list_lowest(descriptor: &AttributeListDescriptor) -> u64 {
    match descriptor.storage {
        ListStorage::Resident(_) => 0,
        ListStorage::NonResident { lowest_vcn, .. } => lowest_vcn,
    }
}
