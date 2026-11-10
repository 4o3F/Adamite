//! Bounded child sorting, top-file selection and extension summary ordering.

use super::aggregate::{has, invalid, overflow, primary_file};
use crate::{
    ExtensionStat, Node, NodeFlags, NodeId, Snapshot, SnapshotError, snapshot::SnapshotStorage,
};
use adamite_ntfs::FileRef;
use std::{
    cmp::{Ordering, Reverse},
    collections::BinaryHeap,
    vec::IntoIter,
};

/// Maximum nodes, directory sorts or extension groups processed per call.
const ITEMS_PER_STEP: usize = 256;
/// Number of primary files retained in the allocation ranking.
const TOP_FILES: usize = 100;

#[derive(Debug, Default)]
enum Stage {
    #[default]
    SortChildren,
    Collect,
    HeapBuild,
    HeapDrain,
    Done,
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct RankedExtension {
    alloc: u64,
    extension: Reverse<String>,
    count: u64,
    size: u64,
}
#[derive(Debug, Default)]
pub(crate) struct Finalizer {
    stage: Stage,
    cursor: usize,
    groups: Vec<ExtensionStat>,
    pending: IntoIter<ExtensionStat>,
    heap: BinaryHeap<RankedExtension>,
}

#[derive(Clone, Copy)]
enum Metric {
    SelfAlloc,
    TotalAlloc,
}

/// Construction validates all column and name ranges before sorting begins.
struct Ranking<'a> {
    nodes: &'a [Node],
    names: &'a [u8],
    refs: &'a [Option<FileRef>],
}
impl Ranking<'_> {
    fn node(&self, id: NodeId) -> Option<&Node> {
        usize::try_from(id.raw())
            .ok()
            .and_then(|id| self.nodes.get(id))
    }
    fn name(&self, id: NodeId) -> Option<&str> {
        let node = self.node(id)?;
        let start = usize::try_from(node.name_off).ok()?;
        let end = start.checked_add(usize::from(node.name_len))?;
        std::str::from_utf8(self.names.get(start..end)?).ok()
    }
    // Order file identities by record number, then sequence number.
    fn reference(&self, id: NodeId) -> Option<(u64, u16)> {
        usize::try_from(id.raw())
            .ok()
            .and_then(|id| self.refs.get(id))
            .copied()
            .flatten()
            .map(|reference| (reference.record(), reference.sequence()))
    }
    fn compare(&self, left: NodeId, right: NodeId, metric: Metric) -> Ordering {
        let alloc = |id| {
            self.node(id).map(|node| match metric {
                Metric::SelfAlloc => node.alloc,
                Metric::TotalAlloc => node.total_alloc,
            })
        };
        alloc(right).cmp(&alloc(left))
            // Valid names reproduce their original UTF-16 ordering without a scratch allocation.
            // Lossy names compare their replacement text, then stable file references and ids.
            .then_with(|| self.name(left).into_iter().flat_map(str::encode_utf16)
                .cmp(self.name(right).into_iter().flat_map(str::encode_utf16)))
            .then_with(|| self.reference(left).cmp(&self.reference(right)))
            .then_with(|| left.cmp(&right))
    }
}

fn extension(name: &str) -> &str {
    match name.rsplit_once('.') {
        Some((stem, suffix)) if !stem.is_empty() && !suffix.is_empty() => suffix,
        _ => "",
    }
}

impl Finalizer {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn sort_children(&mut self, data: &mut SnapshotStorage) -> Result<(), SnapshotError> {
        let Some(node) = data.nodes.get(self.cursor) else {
            self.stage = Stage::Collect;
            self.cursor = 0;
            return Ok(());
        };
        if has(node, NodeFlags::DIRECTORY) || has(node, NodeFlags::SYNTHETIC) {
            let start =
                usize::try_from(node.child_start).map_err(|_| overflow("child range start"))?;
            let end = start
                .checked_add(usize::try_from(node.child_len).map_err(|_| overflow("child count"))?)
                .ok_or_else(|| overflow("child range end"))?;
            let ranking = Ranking {
                nodes: &data.nodes,
                names: &data.names,
                refs: &data.file_refs,
            };
            data.children
                .get_mut(start..end)
                .ok_or_else(|| invalid("invalid child sort range"))?
                .sort_unstable_by(|left, right| ranking.compare(*left, *right, Metric::TotalAlloc));
        }
        self.cursor = self
            .cursor
            .checked_add(1)
            .ok_or_else(|| overflow("directory sort cursor"))?;
        Ok(())
    }

    fn collect(&mut self, data: &mut SnapshotStorage) -> Result<(), SnapshotError> {
        let Some(node) = data.nodes.get(self.cursor) else {
            let count = self.groups.len();
            self.heap
                .try_reserve(count)
                .map_err(|source| SnapshotError::Allocation { source })?;
            data.ext_stats
                .try_reserve_exact(count)
                .map_err(|source| SnapshotError::Allocation { source })?;
            self.pending = std::mem::take(&mut self.groups).into_iter();
            self.stage = Stage::HeapBuild;
            return Ok(());
        };
        let id = NodeId::from_raw(u32::try_from(self.cursor).map_err(|_| overflow("file id"))?);
        self.cursor = self
            .cursor
            .checked_add(1)
            .ok_or_else(|| overflow("file cursor"))?;
        if !primary_file(node) {
            return Ok(());
        }
        let ranking = Ranking {
            nodes: &data.nodes,
            names: &data.names,
            refs: &data.file_refs,
        };
        let position = data
            .top_files
            .binary_search_by(|other| ranking.compare(*other, id, Metric::SelfAlloc))
            .unwrap_or_else(|position| position);
        if position < TOP_FILES {
            if data.top_files.len() == TOP_FILES {
                data.top_files.pop();
            }
            data.top_files
                .try_reserve(1)
                .map_err(|source| SnapshotError::Allocation { source })?;
            data.top_files.insert(position, id);
        }
        let suffix = extension(
            ranking
                .name(id)
                .ok_or_else(|| invalid("missing file name"))?,
        );
        let group = self.groups.binary_search_by(|group| {
            group
                .extension
                .bytes()
                .cmp(suffix.bytes().map(|byte| byte.to_ascii_lowercase()))
        });
        match group {
            Ok(position) => {
                let group = self
                    .groups
                    .get_mut(position)
                    .ok_or_else(|| invalid("missing extension group"))?;
                let count = group
                    .count
                    .checked_add(1)
                    .ok_or_else(|| overflow("extension count"))?;
                let size = group
                    .total_size
                    .checked_add(node.size)
                    .ok_or_else(|| overflow("extension logical bytes"))?;
                let alloc = group
                    .total_alloc
                    .checked_add(node.alloc)
                    .ok_or_else(|| overflow("extension allocation"))?;
                group.count = count;
                group.total_size = size;
                group.total_alloc = alloc;
            }
            Err(position) => {
                let mut key = String::new();
                key.try_reserve_exact(suffix.len())
                    .map_err(|source| SnapshotError::Allocation { source })?;
                for character in suffix.chars() {
                    key.push(character.to_ascii_lowercase());
                }
                self.groups
                    .try_reserve(1)
                    .map_err(|source| SnapshotError::Allocation { source })?;
                self.groups.insert(
                    position,
                    ExtensionStat {
                        extension: key,
                        count: 1,
                        total_size: node.size,
                        total_alloc: node.alloc,
                    },
                );
            }
        }
        Ok(())
    }

    pub(crate) fn step(&mut self, snapshot: &mut Snapshot) -> Result<bool, SnapshotError> {
        for _ in 0..ITEMS_PER_STEP {
            match self.stage {
                Stage::SortChildren => self.sort_children(snapshot.data_mut())?,
                Stage::Collect => self.collect(snapshot.data_mut())?,
                Stage::HeapBuild => {
                    if let Some(group) = self.pending.next() {
                        self.heap.push(RankedExtension {
                            alloc: group.total_alloc,
                            extension: Reverse(group.extension),
                            count: group.count,
                            size: group.total_size,
                        });
                    } else {
                        self.stage = Stage::HeapDrain;
                    }
                }
                Stage::HeapDrain => {
                    if let Some(group) = self.heap.pop() {
                        snapshot.data_mut().ext_stats.push(ExtensionStat {
                            extension: group.extension.0,
                            count: group.count,
                            total_size: group.size,
                            total_alloc: group.alloc,
                        });
                    } else {
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
