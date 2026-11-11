//! Resolve EntryRecord names and parents into a flat parent-index array.

use crate::{
    error::ScanError,
    graph::entry::EntryRecord,
    scan::ScanStats,
    snapshot::{NameRange, NodeFlags, NodeId, ParentTree, ResolvedNode, SnapshotError},
    util::{CounterExt, VecExt},
};
use adamite_ntfs::record::{
    reference::FileRef,
    system::{EXTEND, FIRST_NON_RESERVED, ROOT},
};

/// Maximum graph operations performed before yielding to the caller.
const ITEMS_PER_STEP: usize = 256;

#[derive(Debug, Clone, Copy)]
struct RecordInfo {
    reference: FileRef,
    flags: NodeFlags,
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
    Reorder,
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

impl Linker {
    /// Encodes directly into the shared pool; no node owns an individual name buffer.
    fn append_name(
        pool: &mut Vec<u8>,
        units: impl Iterator<Item = u16> + Clone,
        stats: &mut ScanStats,
    ) -> Result<(u32, u16), ScanError> {
        let len = char::decode_utf16(units.clone()).try_fold(0usize, |len, item| {
            len.checked_add(item.unwrap_or(char::REPLACEMENT_CHARACTER).len_utf8())
                .ok_or_else(ScanError::link_overflow)
        })?;
        let range = NameRange::new(pool.len(), len)?.into_parts();
        pool.try_reserve(len)?;
        for decoded in char::decode_utf16(units) {
            let character = match decoded {
                Ok(character) => character,
                Err(_) => {
                    stats
                        .invalid_utf16
                        .try_bump()
                        .map_err(|_| ScanError::link_overflow())?;
                    char::REPLACEMENT_CHARACTER
                }
            };
            let mut bytes = [0; 4];
            pool.extend_from_slice(character.encode_utf8(&mut bytes).as_bytes());
        }
        Ok(range)
    }

    fn push_bounded<T>(items: &mut Vec<T>, item: T) -> Result<(), ScanError> {
        let len = items
            .len()
            .checked_add(1)
            .ok_or_else(ScanError::link_overflow)?;
        u32::try_from(len).map_err(|_| SnapshotError::overflow("link storage length"))?;
        items.try_push(item)?;
        Ok(())
    }

    pub(crate) fn new(entries: Vec<EntryRecord>) -> Result<Self, ScanError> {
        let mut nodes = Vec::new();
        let capacity = entries
            .len()
            .checked_add(3)
            .ok_or_else(ScanError::link_overflow)?;
        nodes.try_reserve_exact(capacity)?;
        let mut records = Vec::new();
        records.try_reserve_exact(entries.len())?;
        Self::push_bounded(&mut nodes, ResolvedNode::synthetic())?;
        Ok(Self {
            records,
            sources: entries,
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

    fn prepare(&mut self, stats: &mut ScanStats) -> Result<(), ScanError> {
        let entry = self
            .sources
            .get_mut(self.cursor)
            .ok_or_else(ScanError::link_overflow)?;
        let ordinal = entry.file_ref.record();
        let index = match self
            .records
            .binary_search_by_key(&ordinal, |record| record.reference.record())
        {
            Ok(_) if ordinal == ROOT => {
                return Err(ScanError::RequiredRecord { record: ROOT });
            }
            Ok(_) => return Err(ScanError::link("duplicate entry record identity")),
            Err(index) => index,
        };
        self.records.try_reserve(1)?;
        let (primary, len) = if ordinal == ROOT {
            if !entry.flags.contains(NodeFlags::DIRECTORY) {
                return Err(ScanError::RequiredRecord { record: ROOT });
            }
            *self
                .nodes
                .get_mut(0)
                .ok_or_else(|| ScanError::link("missing root placeholder"))? =
                entry.resolved_node(None);
            self.root_seen = true;
            (0, 1)
        } else {
            entry.select_names();
            let names = &entry.names;
            let primary = self.nodes.len();
            if names.is_empty() {
                Self::push_bounded(&mut self.nodes, entry.resolved_node(None))?;
                (primary, 1)
            } else {
                let len = names.len();
                for (index, name) in names.iter().enumerate() {
                    let mut pending = entry.resolved_node(Some(name.parent));
                    if index != 0 {
                        pending.flags.insert(NodeFlags::HARDLINK_DUP);
                        stats
                            .hardlink_duplicates
                            .try_bump()
                            .map_err(|_| ScanError::link_overflow())?;
                    }
                    Self::push_bounded(&mut self.nodes, pending)?;
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
                source: self.cursor,
            },
        );
        self.cursor = self
            .cursor
            .checked_add(1)
            .ok_or_else(ScanError::link_overflow)?;
        Ok(())
    }

    fn ensure_metadata(&mut self) -> Result<usize, ScanError> {
        if let Some(id) = self.metadata {
            return Ok(id);
        }
        let id = self.nodes.len();
        Self::push_bounded(&mut self.nodes, ResolvedNode::synthetic())?;
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
            self.marks.try_reserve(1)?;
            self.positions.try_reserve(1)?;
        }
        Self::push_bounded(&mut self.nodes, ResolvedNode::synthetic())?;
        if !self.marks.is_empty() {
            self.marks.push(2);
            self.positions.push(None);
        }
        self.orphaned = Some(id);
        Ok(id)
    }

    fn mark_metadata(&mut self, index: usize, stats: &mut ScanStats) -> Result<(), ScanError> {
        let record = self
            .records
            .get(index)
            .copied()
            .ok_or_else(ScanError::link_overflow)?;
        if record.reference.record() == ROOT {
            return Ok(());
        }
        let reserved = record.reference.record() < FIRST_NON_RESERVED;
        let extend = self
            .record(EXTEND)
            .filter(|record| record.flags.contains(NodeFlags::DIRECTORY));
        let end = record
            .primary
            .checked_add(record.len)
            .ok_or_else(ScanError::link_overflow)?;
        let direct = self
            .nodes
            .get(record.primary..end)
            .ok_or_else(ScanError::link_overflow)?
            .iter()
            .any(|node| {
                extend.is_some_and(|extend| node.original_parent == Some(extend.reference))
            });
        if reserved || direct {
            self.ensure_metadata()?;
            self.records
                .get_mut(index)
                .ok_or_else(ScanError::link_overflow)?
                .metadata = true;
            stats
                .metafiles
                .try_bump()
                .map_err(|_| ScanError::link_overflow())?;
        }
        Ok(())
    }

    fn resolve(&mut self, index: usize, stats: &mut ScanStats) -> Result<(), ScanError> {
        if index == 0 {
            return Ok(());
        }
        let node = self.nodes.get(index).ok_or_else(ScanError::link_overflow)?;
        let Some(reference) = node.file_ref else {
            return Ok(());
        };
        let owner = self
            .record(reference.record())
            .ok_or_else(|| ScanError::link("node has no owning entry"))?;
        let parent = if owner.metadata {
            self.metadata
                .ok_or_else(|| ScanError::link("metadata directory was not created"))?
        } else if let Some(parent) = node.original_parent.and_then(|reference| {
            self.record(reference.record()).filter(|parent| {
                parent.reference == reference && parent.flags.contains(NodeFlags::DIRECTORY)
            })
        }) {
            parent.primary
        } else {
            stats
                .orphans
                .try_bump()
                .map_err(|_| ScanError::link_overflow())?;
            self.ensure_orphaned()?
        };
        self.nodes
            .get_mut(index)
            .ok_or_else(ScanError::link_overflow)?
            .parent = parent;
        Ok(())
    }

    fn start_cycles(&mut self) -> Result<(), ScanError> {
        self.marks.try_reserve_exact(self.nodes.len())?;
        self.positions.try_reserve_exact(self.nodes.len())?;
        self.marks.resize(self.nodes.len(), 0);
        self.positions.resize(self.nodes.len(), None);
        *self.marks.get_mut(0).ok_or_else(ScanError::link_overflow)? = 2;
        self.cursor = 1;
        self.stage = Stage::Cycles;
        Ok(())
    }

    fn visit(&mut self, id: usize) -> Result<(), ScanError> {
        let position = self.path.len();
        Self::push_bounded(&mut self.path, id)?;
        *self
            .marks
            .get_mut(id)
            .ok_or_else(ScanError::link_overflow)? = 1;
        *self
            .positions
            .get_mut(id)
            .ok_or_else(ScanError::link_overflow)? = Some(position);
        Ok(())
    }

    fn cycle_step(&mut self, stats: &mut ScanStats) -> Result<(), ScanError> {
        match self.cycle_work {
            CycleWork::Start => {
                if self.cursor == self.marks.len() {
                    self.marks = Vec::new();
                    self.positions = Vec::new();
                    self.path = Vec::new();
                    self.order.try_reserve_exact(self.nodes.len())?;
                    self.remap.try_reserve_exact(self.nodes.len())?;
                    self.remap.resize(self.nodes.len(), 0);
                    self.stage = Stage::Convert;
                    self.cursor = 0;
                    return Ok(());
                }
                if self.marks.get(self.cursor) == Some(&0) {
                    self.visit(self.cursor)?;
                    self.cycle_work = CycleWork::Walk;
                } else {
                    self.cursor = self
                        .cursor
                        .checked_add(1)
                        .ok_or_else(ScanError::link_overflow)?;
                }
            }
            CycleWork::Walk => {
                let id = self
                    .path
                    .last()
                    .copied()
                    .ok_or_else(|| ScanError::link("empty cycle walk"))?;
                let parent = self
                    .nodes
                    .get(id)
                    .ok_or_else(ScanError::link_overflow)?
                    .parent;
                match self
                    .marks
                    .get(parent)
                    .copied()
                    .ok_or_else(ScanError::link_overflow)?
                {
                    0 => self.visit(parent)?,
                    1 => {
                        self.cycle_work =
                            CycleWork::Break {
                                cursor: self.positions.get(parent).copied().flatten().ok_or_else(
                                    || ScanError::link("cycle has no stack position"),
                                )?,
                            }
                    }
                    _ => self.cycle_work = CycleWork::Unwind,
                }
            }
            CycleWork::Break { cursor } => {
                if let Some(id) = self.path.get(cursor).copied() {
                    let orphaned = self.ensure_orphaned()?;
                    let node = self
                        .nodes
                        .get_mut(id)
                        .ok_or_else(ScanError::link_overflow)?;
                    if !node.flags.contains(NodeFlags::DIRECTORY) {
                        return Err(ScanError::link("non-directory node in parent cycle"));
                    }
                    node.parent = orphaned;
                    stats
                        .cycle_nodes
                        .try_bump()
                        .map_err(|_| ScanError::link_overflow())?;
                    stats
                        .orphans
                        .try_bump()
                        .map_err(|_| ScanError::link_overflow())?;
                    self.cycle_work = CycleWork::Break {
                        cursor: cursor.checked_add(1).ok_or_else(ScanError::link_overflow)?,
                    };
                } else {
                    self.cycle_work = CycleWork::Unwind;
                }
            }
            CycleWork::Unwind => {
                if let Some(id) = self.path.pop() {
                    *self
                        .marks
                        .get_mut(id)
                        .ok_or_else(ScanError::link_overflow)? = 2;
                    *self
                        .positions
                        .get_mut(id)
                        .ok_or_else(ScanError::link_overflow)? = None;
                } else {
                    self.cursor = self
                        .cursor
                        .checked_add(1)
                        .ok_or_else(ScanError::link_overflow)?;
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
            .ok_or_else(ScanError::link_overflow)?;
        let old = if self.cursor == 0 {
            0
        } else if self.cursor < synthetic_count {
            if self.cursor == 1 {
                self.metadata
                    .or(self.orphaned)
                    .ok_or_else(ScanError::link_overflow)?
            } else {
                self.orphaned.ok_or_else(ScanError::link_overflow)?
            }
        } else {
            // Records are already indexed in ascending ordinal order.
            let mut record = self
                .records
                .get(self.order_record)
                .ok_or_else(ScanError::link_overflow)?;
            if record.reference.record() == ROOT {
                self.order_record = self
                    .order_record
                    .checked_add(1)
                    .ok_or_else(ScanError::link_overflow)?;
                record = self
                    .records
                    .get(self.order_record)
                    .ok_or_else(ScanError::link_overflow)?;
            }
            let old = record
                .primary
                .checked_add(self.order_link)
                .ok_or_else(ScanError::link_overflow)?;
            self.order_link = self
                .order_link
                .checked_add(1)
                .ok_or_else(ScanError::link_overflow)?;
            if self.order_link == record.len {
                self.order_record = self
                    .order_record
                    .checked_add(1)
                    .ok_or_else(ScanError::link_overflow)?;
                self.order_link = 0;
            }
            old
        };
        *self
            .remap
            .get_mut(old)
            .ok_or_else(ScanError::link_overflow)? =
            NodeId::try_from_index(self.cursor, "node id")?.raw();
        Self::push_bounded(&mut self.order, old)?;
        self.cursor = self
            .cursor
            .checked_add(1)
            .ok_or_else(ScanError::link_overflow)?;
        Ok(())
    }

    fn emit(&mut self, stats: &mut ScanStats) -> Result<(), ScanError> {
        let old = *self
            .order
            .get(self.cursor)
            .ok_or_else(ScanError::link_overflow)?;
        let mut node = *self.nodes.get(old).ok_or_else(ScanError::link_overflow)?;
        let range = if old == 0 {
            Self::append_name(&mut self.result.names, "/".encode_utf16(), stats)?
        } else if Some(old) == self.metadata {
            Self::append_name(
                &mut self.result.names,
                "<NTFS metadata>".encode_utf16(),
                stats,
            )?
        } else if Some(old) == self.orphaned {
            Self::append_name(&mut self.result.names, "<orphaned>".encode_utf16(), stats)?
        } else {
            let record = self
                .record(node.file_ref.ok_or_else(ScanError::link_overflow)?.record())
                .ok_or_else(ScanError::link_overflow)?;
            let source = self
                .sources
                .get(record.source)
                .ok_or_else(ScanError::link_overflow)?;
            if source.names.is_empty() {
                Self::append_name(
                    &mut self.result.names,
                    source.fallback_name()?.encode_utf16(),
                    stats,
                )?
            } else {
                let index = old
                    .checked_sub(record.primary)
                    .ok_or_else(ScanError::link_overflow)?;
                let name = source
                    .names
                    .get(index)
                    .ok_or_else(ScanError::link_overflow)?;
                Self::append_name(&mut self.result.names, name.name.iter().copied(), stats)?
            }
        };
        if let Some(reference) = node.file_ref {
            let record = self
                .record(reference.record())
                .ok_or_else(ScanError::link_overflow)?;
            if old.checked_add(1).ok_or_else(ScanError::link_overflow)?
                == record
                    .primary
                    .checked_add(record.len)
                    .ok_or_else(ScanError::link_overflow)?
            {
                self.sources
                    .get_mut(record.source)
                    .ok_or_else(ScanError::link_overflow)?
                    .names = Vec::new();
            }
        }
        node.name_off = range.0;
        node.name_len = range.1;
        node.parent = usize::try_from(
            *self
                .remap
                .get(node.parent)
                .ok_or_else(ScanError::link_overflow)?,
        )
        .map_err(|_| ScanError::link_overflow())?;
        *self
            .nodes
            .get_mut(old)
            .ok_or_else(ScanError::link_overflow)? = node;
        self.cursor = self
            .cursor
            .checked_add(1)
            .ok_or_else(ScanError::link_overflow)?;
        Ok(())
    }

    pub(crate) fn step(&mut self, stats: &mut ScanStats) -> Result<bool, ScanError> {
        for _ in 0..ITEMS_PER_STEP {
            match self.stage {
                Stage::Prepare => {
                    if self.cursor < self.sources.len() {
                        self.prepare(stats)?;
                    } else {
                        if !self.root_seen {
                            return Err(ScanError::RequiredRecord { record: ROOT });
                        }
                        self.stage = Stage::Metadata;
                        self.cursor = 0;
                    }
                }
                Stage::Metadata => {
                    if self.cursor < self.records.len() {
                        self.mark_metadata(self.cursor, stats)?;
                        self.cursor = self
                            .cursor
                            .checked_add(1)
                            .ok_or_else(ScanError::link_overflow)?;
                    } else {
                        self.stage = Stage::Resolve;
                        self.cursor = 0;
                    }
                }
                Stage::Resolve => {
                    if self.cursor < self.nodes.len() {
                        self.resolve(self.cursor, stats)?;
                        self.cursor = self
                            .cursor
                            .checked_add(1)
                            .ok_or_else(ScanError::link_overflow)?;
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
                        self.order = Vec::new();
                        self.stage = Stage::Reorder;
                        self.cursor = 0;
                    }
                }
                Stage::Reorder => {
                    // All parents now use final ids; reuse the old-to-new map for in-place swaps.
                    if self.cursor == self.nodes.len() {
                        self.result.nodes = std::mem::take(&mut self.nodes);
                        self.stage = Stage::Done;
                        return Ok(true);
                    }
                    let destination = usize::try_from(
                        *self
                            .remap
                            .get(self.cursor)
                            .ok_or_else(ScanError::link_overflow)?,
                    )
                    .map_err(|_| ScanError::link_overflow())?;
                    if destination == self.cursor {
                        self.cursor = self
                            .cursor
                            .checked_add(1)
                            .ok_or_else(ScanError::link_overflow)?;
                    } else {
                        if destination >= self.nodes.len() || destination >= self.remap.len() {
                            return Err(ScanError::link("node permutation outside arena"));
                        }
                        self.nodes.swap(self.cursor, destination);
                        self.remap.swap(self.cursor, destination);
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
