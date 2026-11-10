//! Resolve EntryRecord names and parents into a flat parent-index array.

use crate::{
    ScanError, ScanStats,
    entry::{EntryFlags, EntryName, EntryRecord},
    snapshot::{checked_u32, name_range},
};
use adamite_ntfs::{EXTEND_RECORD, FIRST_NON_RESERVED_RECORD, FileRef, ROOT_RECORD};
use std::{fmt::Write, vec::IntoIter};

/// Maximum graph operations performed before yielding to the caller.
const ITEMS_PER_STEP: usize = 256;

/// One node in a flat parent-index array.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ResolvedNode {
    pub parent: usize,
    pub file_ref: Option<FileRef>,
    pub original_parent: Option<FileRef>,
    pub name_off: u32,
    pub name_len: u16,
    pub flags: EntryFlags,
    pub attrs: u32,
    pub mtime: u64,
    pub size: u64,
    pub ads_size: u64,
    pub alloc: u64,
    pub nlink: u16,
    pub reparse_tag: Option<u32>,
}

/// A flat Vec of nodes whose parent fields are indices into the same array.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct ParentTree {
    pub nodes: Vec<ResolvedNode>,
    pub names: Vec<u8>,
}

#[derive(Debug, Clone, Copy)]
struct RecordInfo {
    reference: FileRef,
    flags: EntryFlags,
    primary: usize,
    len: usize,
    metadata: bool,
    source: usize,
}

#[derive(Debug)]
enum Stage {
    Prepare,
    Metadata,
    Resolve,
    Cycles,
    Convert,
    Emit,
    Done,
}
#[derive(Debug)]
enum CycleWork {
    Start,
    Walk,
    Break { cursor: usize },
    Unwind,
}

#[derive(Debug)]
pub(crate) struct Linker {
    entries: IntoIter<EntryRecord>,
    records: Vec<RecordInfo>,
    sources: Vec<EntryRecord>,
    order: Vec<usize>,
    remap: Vec<u32>,
    order_record: usize,
    order_link: usize,
    nodes: Vec<ResolvedNode>,
    root_seen: bool,
    metadata: Option<usize>,
    orphaned: Option<usize>,
    stage: Stage,
    cursor: usize,
    marks: Vec<u8>,
    positions: Vec<Option<usize>>,
    path: Vec<usize>,
    cycle_work: CycleWork,
    pub result: ParentTree,
}

fn invalid(reason: &'static str) -> ScanError {
    ScanError::Link { reason }
}
fn overflow() -> ScanError {
    ScanError::Overflow {
        operation: "tree linking",
    }
}
fn increment(counter: &mut u64) -> Result<(), ScanError> {
    *counter = counter.checked_add(1).ok_or_else(overflow)?;
    Ok(())
}
fn push<T>(items: &mut Vec<T>, item: T) -> Result<(), ScanError> {
    checked_u32(
        items.len().checked_add(1).ok_or_else(overflow)?,
        "link storage length",
    )?;
    items
        .try_reserve(1)
        .map_err(|source| ScanError::Allocation { source })?;
    items.push(item);
    Ok(())
}
fn unnamed(record: u64) -> Result<String, ScanError> {
    let mut text = String::new();
    // A 48-bit ordinal needs at most 15 digits, plus the ASCII label and delimiters.
    text.try_reserve_exact(32)
        .map_err(|source| ScanError::Allocation { source })?;
    write!(&mut text, "<record {record}>")
        .map_err(|_| invalid("could not format unnamed record"))?;
    Ok(text)
}
fn synthetic() -> ResolvedNode {
    let mut flags = EntryFlags::DIRECTORY;
    flags.insert(EntryFlags::SYNTHETIC);
    ResolvedNode {
        parent: 0,
        file_ref: None,
        original_parent: None,
        name_off: 0,
        name_len: 0,
        flags,
        attrs: 0,
        mtime: 0,
        size: 0,
        ads_size: 0,
        alloc: 0,
        nlink: 0,
        reparse_tag: None,
    }
}
fn node(entry: &EntryRecord, parent_reference: Option<FileRef>) -> ResolvedNode {
    ResolvedNode {
        parent: 0,
        file_ref: Some(entry.file_ref),
        original_parent: parent_reference,
        name_off: 0,
        name_len: 0,
        flags: entry.flags,
        attrs: entry.attrs,
        mtime: entry.mtime,
        size: entry.size,
        ads_size: entry.ads_size,
        alloc: entry.alloc,
        nlink: entry.nlink,
        reparse_tag: entry.reparse_tag,
    }
}

/// Select aliases before decoding; equal parent/name links keep the smallest namespace.
fn select_names(mut names: Vec<EntryName>) -> Vec<EntryName> {
    names.sort_unstable_by(|a, b| {
        (
            a.parent.record(),
            a.parent.sequence(),
            a.namespace == 2,
            a.namespace,
            &a.name,
        )
            .cmp(&(
                b.parent.record(),
                b.parent.sequence(),
                b.namespace == 2,
                b.namespace,
                &b.name,
            ))
    });
    let mut parent = None;
    let mut non_dos = false;
    names.retain(|name| {
        if parent != Some(name.parent) {
            parent = Some(name.parent);
            non_dos = false;
        }
        if name.namespace != 2 {
            non_dos = true;
        }
        name.namespace != 2 || !non_dos
    });
    names.sort_unstable_by(|a, b| {
        (a.parent.record(), a.parent.sequence(), &a.name, a.namespace).cmp(&(
            b.parent.record(),
            b.parent.sequence(),
            &b.name,
            b.namespace,
        ))
    });
    names.dedup_by(|a, b| a.parent == b.parent && a.name == b.name);
    names.sort_unstable_by(|a, b| {
        (a.parent.record(), &a.name, a.namespace, a.parent.sequence()).cmp(&(
            b.parent.record(),
            &b.name,
            b.namespace,
            b.parent.sequence(),
        ))
    });
    names
}

/// Encodes directly into the shared pool; no node owns an individual name buffer.
fn append_name(
    pool: &mut Vec<u8>,
    units: impl Iterator<Item = u16> + Clone,
    stats: &mut ScanStats,
) -> Result<(u32, u16), ScanError> {
    let len = char::decode_utf16(units.clone()).try_fold(0usize, |len, item| {
        len.checked_add(item.unwrap_or(char::REPLACEMENT_CHARACTER).len_utf8())
            .ok_or_else(overflow)
    })?;
    let range = name_range(pool.len(), len)?;
    pool.try_reserve(len)
        .map_err(|source| ScanError::Allocation { source })?;
    for decoded in char::decode_utf16(units) {
        let character = match decoded {
            Ok(character) => character,
            Err(_) => {
                increment(&mut stats.invalid_utf16)?;
                char::REPLACEMENT_CHARACTER
            }
        };
        let mut bytes = [0; 4];
        pool.extend_from_slice(character.encode_utf8(&mut bytes).as_bytes());
    }
    Ok(range)
}

impl Linker {
    pub(crate) fn new(entries: Vec<EntryRecord>) -> Result<Self, ScanError> {
        let mut nodes = Vec::new();
        push(&mut nodes, synthetic())?;
        Ok(Self {
            entries: entries.into_iter(),
            records: Vec::new(),
            sources: Vec::new(),
            order: Vec::new(),
            remap: Vec::new(),
            order_record: 0,
            order_link: 0,
            nodes,
            root_seen: false,
            metadata: None,
            orphaned: None,
            stage: Stage::Prepare,
            cursor: 0,
            marks: Vec::new(),
            positions: Vec::new(),
            path: Vec::new(),
            cycle_work: CycleWork::Start,
            result: ParentTree::default(),
        })
    }

    fn record(&self, ordinal: u64) -> Option<RecordInfo> {
        let index = self
            .records
            .binary_search_by_key(&ordinal, |record| record.reference.record())
            .ok()?;
        self.records.get(index).copied()
    }

    fn prepare(&mut self, mut entry: EntryRecord, stats: &mut ScanStats) -> Result<(), ScanError> {
        let ordinal = entry.file_ref.record();
        let index = match self
            .records
            .binary_search_by_key(&ordinal, |record| record.reference.record())
        {
            Ok(_) if ordinal == ROOT_RECORD => {
                return Err(ScanError::RequiredRecord {
                    record: ROOT_RECORD,
                });
            }
            Ok(_) => return Err(invalid("duplicate entry record identity")),
            Err(index) => index,
        };
        self.records
            .try_reserve(1)
            .map_err(|source| ScanError::Allocation { source })?;
        let (primary, len) = if ordinal == ROOT_RECORD {
            if !entry.flags.contains(EntryFlags::DIRECTORY) {
                return Err(ScanError::RequiredRecord {
                    record: ROOT_RECORD,
                });
            }
            *self
                .nodes
                .get_mut(0)
                .ok_or_else(|| invalid("missing root placeholder"))? = node(&entry, None);
            self.root_seen = true;
            (0, 1)
        } else {
            entry.names = select_names(std::mem::take(&mut entry.names));
            let names = &entry.names;
            let primary = self.nodes.len();
            if names.is_empty() {
                push(&mut self.nodes, node(&entry, None))?;
                (primary, 1)
            } else {
                let len = names.len();
                for (index, name) in names.iter().enumerate() {
                    let mut pending = node(&entry, Some(name.parent));
                    if index != 0 {
                        pending.flags.insert(EntryFlags::HARDLINK_DUP);
                        increment(&mut stats.hardlink_duplicates)?;
                    }
                    push(&mut self.nodes, pending)?;
                }
                (primary, len)
            }
        };
        self.records.insert(
            index,
            RecordInfo {
                reference: entry.file_ref,
                flags: entry.flags,
                primary,
                len,
                metadata: false,
                source: self.sources.len(),
            },
        );
        push(&mut self.sources, entry)?;
        Ok(())
    }

    fn ensure_metadata(&mut self) -> Result<usize, ScanError> {
        if let Some(id) = self.metadata {
            return Ok(id);
        }
        let id = self.nodes.len();
        push(&mut self.nodes, synthetic())?;
        self.metadata = Some(id);
        Ok(id)
    }

    fn ensure_orphaned(&mut self) -> Result<usize, ScanError> {
        if let Some(id) = self.orphaned {
            return Ok(id);
        }
        let id = self.nodes.len();
        // Cycle detection may discover the first orphan after its state arrays exist.
        if !self.marks.is_empty() {
            self.marks
                .try_reserve(1)
                .map_err(|source| ScanError::Allocation { source })?;
            self.positions
                .try_reserve(1)
                .map_err(|source| ScanError::Allocation { source })?;
        }
        push(&mut self.nodes, synthetic())?;
        if !self.marks.is_empty() {
            self.marks.push(2);
            self.positions.push(None);
        }
        self.orphaned = Some(id);
        Ok(id)
    }

    fn mark_metadata(&mut self, index: usize, stats: &mut ScanStats) -> Result<(), ScanError> {
        let record = self.records.get(index).copied().ok_or_else(overflow)?;
        if record.reference.record() == ROOT_RECORD {
            return Ok(());
        }
        let reserved = record.reference.record() < FIRST_NON_RESERVED_RECORD;
        let extend = self
            .record(EXTEND_RECORD)
            .filter(|record| record.flags.contains(EntryFlags::DIRECTORY));
        let end = record
            .primary
            .checked_add(record.len)
            .ok_or_else(overflow)?;
        let direct = self
            .nodes
            .get(record.primary..end)
            .ok_or_else(overflow)?
            .iter()
            .any(|node| {
                extend.is_some_and(|extend| node.original_parent == Some(extend.reference))
            });
        if reserved || direct {
            self.ensure_metadata()?;
            self.records.get_mut(index).ok_or_else(overflow)?.metadata = true;
            increment(&mut stats.metafiles)?;
        }
        Ok(())
    }

    fn resolve(&mut self, index: usize, stats: &mut ScanStats) -> Result<(), ScanError> {
        if index == 0 {
            return Ok(());
        }
        let node = self.nodes.get(index).ok_or_else(overflow)?;
        let Some(reference) = node.file_ref else {
            return Ok(());
        };
        let owner = self
            .record(reference.record())
            .ok_or_else(|| invalid("node has no owning entry"))?;
        let parent = if owner.metadata {
            self.metadata
                .ok_or_else(|| invalid("metadata directory was not created"))?
        } else if let Some(parent) = node.original_parent.and_then(|reference| {
            self.record(reference.record()).filter(|parent| {
                parent.reference == reference && parent.flags.contains(EntryFlags::DIRECTORY)
            })
        }) {
            parent.primary
        } else {
            increment(&mut stats.orphans)?;
            self.ensure_orphaned()?
        };
        self.nodes.get_mut(index).ok_or_else(overflow)?.parent = parent;
        Ok(())
    }

    fn start_cycles(&mut self) -> Result<(), ScanError> {
        self.marks
            .try_reserve_exact(self.nodes.len())
            .map_err(|source| ScanError::Allocation { source })?;
        self.positions
            .try_reserve_exact(self.nodes.len())
            .map_err(|source| ScanError::Allocation { source })?;
        self.marks.resize(self.nodes.len(), 0);
        self.positions.resize(self.nodes.len(), None);
        *self.marks.get_mut(0).ok_or_else(overflow)? = 2;
        self.cursor = 1;
        self.stage = Stage::Cycles;
        Ok(())
    }

    fn visit(&mut self, id: usize) -> Result<(), ScanError> {
        let position = self.path.len();
        push(&mut self.path, id)?;
        *self.marks.get_mut(id).ok_or_else(overflow)? = 1;
        *self.positions.get_mut(id).ok_or_else(overflow)? = Some(position);
        Ok(())
    }

    fn cycle_step(&mut self, stats: &mut ScanStats) -> Result<(), ScanError> {
        match self.cycle_work {
            CycleWork::Start => {
                if self.cursor == self.marks.len() {
                    self.remap
                        .try_reserve_exact(self.nodes.len())
                        .map_err(|source| ScanError::Allocation { source })?;
                    self.remap.resize(self.nodes.len(), 0);
                    self.stage = Stage::Convert;
                    self.cursor = 0;
                    return Ok(());
                }
                if self.marks.get(self.cursor) == Some(&0) {
                    self.visit(self.cursor)?;
                    self.cycle_work = CycleWork::Walk;
                } else {
                    self.cursor = self.cursor.checked_add(1).ok_or_else(overflow)?;
                }
            }
            CycleWork::Walk => {
                let id = self
                    .path
                    .last()
                    .copied()
                    .ok_or_else(|| invalid("empty cycle walk"))?;
                let parent = self.nodes.get(id).ok_or_else(overflow)?.parent;
                match self.marks.get(parent).copied().ok_or_else(overflow)? {
                    0 => self.visit(parent)?,
                    1 => {
                        self.cycle_work = CycleWork::Break {
                            cursor: self
                                .positions
                                .get(parent)
                                .copied()
                                .flatten()
                                .ok_or_else(|| invalid("cycle has no stack position"))?,
                        }
                    }
                    _ => self.cycle_work = CycleWork::Unwind,
                }
            }
            CycleWork::Break { cursor } => {
                if let Some(id) = self.path.get(cursor).copied() {
                    let orphaned = self.ensure_orphaned()?;
                    let node = self.nodes.get_mut(id).ok_or_else(overflow)?;
                    if !node.flags.contains(EntryFlags::DIRECTORY) {
                        return Err(invalid("non-directory node in parent cycle"));
                    }
                    node.parent = orphaned;
                    increment(&mut stats.cycle_nodes)?;
                    increment(&mut stats.orphans)?;
                    self.cycle_work = CycleWork::Break {
                        cursor: cursor.checked_add(1).ok_or_else(overflow)?,
                    };
                } else {
                    self.cycle_work = CycleWork::Unwind;
                }
            }
            CycleWork::Unwind => {
                if let Some(id) = self.path.pop() {
                    *self.marks.get_mut(id).ok_or_else(overflow)? = 2;
                    *self.positions.get_mut(id).ok_or_else(overflow)? = None;
                } else {
                    self.cursor = self.cursor.checked_add(1).ok_or_else(overflow)?;
                    self.cycle_work = CycleWork::Start;
                }
            }
        }
        Ok(())
    }

    fn order_one(&mut self) -> Result<(), ScanError> {
        let synthetic_count = 1usize
            .checked_add(usize::from(self.metadata.is_some()))
            .and_then(|n| n.checked_add(usize::from(self.orphaned.is_some())))
            .ok_or_else(overflow)?;
        let old = if self.cursor == 0 {
            0
        } else if self.cursor < synthetic_count {
            if self.cursor == 1 {
                self.metadata.or(self.orphaned).ok_or_else(overflow)?
            } else {
                self.orphaned.ok_or_else(overflow)?
            }
        } else {
            // Records are already indexed in ascending ordinal order.
            let mut record = self.records.get(self.order_record).ok_or_else(overflow)?;
            if record.reference.record() == ROOT_RECORD {
                self.order_record = self.order_record.checked_add(1).ok_or_else(overflow)?;
                record = self.records.get(self.order_record).ok_or_else(overflow)?;
            }
            let old = record
                .primary
                .checked_add(self.order_link)
                .ok_or_else(overflow)?;
            self.order_link = self.order_link.checked_add(1).ok_or_else(overflow)?;
            if self.order_link == record.len {
                self.order_record = self.order_record.checked_add(1).ok_or_else(overflow)?;
                self.order_link = 0;
            }
            old
        };
        *self.remap.get_mut(old).ok_or_else(overflow)? = checked_u32(self.cursor, "node id")?;
        push(&mut self.order, old)?;
        self.cursor = self.cursor.checked_add(1).ok_or_else(overflow)?;
        Ok(())
    }

    fn emit(&mut self, stats: &mut ScanStats) -> Result<(), ScanError> {
        let old = *self.order.get(self.cursor).ok_or_else(overflow)?;
        let mut node = *self.nodes.get(old).ok_or_else(overflow)?;
        let range = if old == 0 {
            append_name(&mut self.result.names, "/".encode_utf16(), stats)?
        } else if Some(old) == self.metadata {
            append_name(
                &mut self.result.names,
                "<NTFS metadata>".encode_utf16(),
                stats,
            )?
        } else if Some(old) == self.orphaned {
            append_name(&mut self.result.names, "<orphaned>".encode_utf16(), stats)?
        } else {
            let record = self
                .record(node.file_ref.ok_or_else(overflow)?.record())
                .ok_or_else(overflow)?;
            let source = self.sources.get(record.source).ok_or_else(overflow)?;
            if source.names.is_empty() {
                append_name(
                    &mut self.result.names,
                    unnamed(record.reference.record())?.encode_utf16(),
                    stats,
                )?
            } else {
                let index = old.checked_sub(record.primary).ok_or_else(overflow)?;
                let name = source.names.get(index).ok_or_else(overflow)?;
                append_name(&mut self.result.names, name.name.iter().copied(), stats)?
            }
        };
        if let Some(reference) = node.file_ref {
            let record = self.record(reference.record()).ok_or_else(overflow)?;
            if old.checked_add(1).ok_or_else(overflow)?
                == record
                    .primary
                    .checked_add(record.len)
                    .ok_or_else(overflow)?
            {
                self.sources
                    .get_mut(record.source)
                    .ok_or_else(overflow)?
                    .names = Vec::new();
            }
        }
        node.name_off = range.0;
        node.name_len = range.1;
        node.parent = usize::try_from(*self.remap.get(node.parent).ok_or_else(overflow)?)
            .map_err(|_| overflow())?;
        push(&mut self.result.nodes, node)?;
        self.cursor = self.cursor.checked_add(1).ok_or_else(overflow)?;
        Ok(())
    }

    pub(crate) fn step(&mut self, stats: &mut ScanStats) -> Result<bool, ScanError> {
        for _ in 0..ITEMS_PER_STEP {
            match self.stage {
                Stage::Prepare => {
                    if let Some(entry) = self.entries.next() {
                        self.prepare(entry, stats)?;
                    } else {
                        if !self.root_seen {
                            return Err(ScanError::RequiredRecord {
                                record: ROOT_RECORD,
                            });
                        }
                        self.stage = Stage::Metadata;
                        self.cursor = 0;
                    }
                }
                Stage::Metadata => {
                    if self.cursor < self.records.len() {
                        self.mark_metadata(self.cursor, stats)?;
                        self.cursor = self.cursor.checked_add(1).ok_or_else(overflow)?;
                    } else {
                        self.stage = Stage::Resolve;
                        self.cursor = 0;
                    }
                }
                Stage::Resolve => {
                    if self.cursor < self.nodes.len() {
                        self.resolve(self.cursor, stats)?;
                        self.cursor = self.cursor.checked_add(1).ok_or_else(overflow)?;
                    } else {
                        self.start_cycles()?;
                    }
                }
                Stage::Cycles => self.cycle_step(stats)?,
                Stage::Convert => {
                    if self.cursor < self.nodes.len() {
                        self.order_one()?;
                    } else {
                        self.stage = Stage::Emit;
                        self.cursor = 0;
                    }
                }
                Stage::Emit => {
                    if self.cursor < self.order.len() {
                        self.emit(stats)?;
                    } else {
                        self.sources = Vec::new();
                        self.stage = Stage::Done;
                        return Ok(true);
                    }
                }
                Stage::Done => return Ok(true),
            }
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests;
