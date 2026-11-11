//! Bounded child sorting, top-file selection and extension summary ordering.

use crate::snapshot::rules::{Extension, Metric, Ranking, TOP_FILES};
use crate::snapshot::{
    NodeFlags, NodeId, Snapshot, SnapshotError, indexes::ExtensionGroups, storage::SnapshotStorage,
};

/// Maximum nodes or extension groups per call; each directory sort yields immediately.
const ITEMS_PER_STEP: usize = 256;

#[derive(Debug, Default)]
enum Stage {
    #[default]
    SortChildren,
    Collect,
    HeapBuild,
    HeapDrain,
    Done,
}
#[derive(Debug, Default)]
pub(crate) struct Finalizer {
    stage: Stage,
    cursor: usize,
    groups: ExtensionGroups,
}

impl Finalizer {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn sort_children(&mut self, data: &mut SnapshotStorage) -> Result<bool, SnapshotError> {
        let Some(node) = data.nodes.get(self.cursor) else {
            self.stage = Stage::Collect;
            self.cursor = 0;
            return Ok(false);
        };
        let directory = node.has(NodeFlags::DIRECTORY) || node.has(NodeFlags::SYNTHETIC);
        if directory {
            let start = usize::try_from(node.child_start)
                .map_err(|_| SnapshotError::overflow("child range start"))?;
            let end = start
                .checked_add(
                    usize::try_from(node.child_len)
                        .map_err(|_| SnapshotError::overflow("child count"))?,
                )
                .ok_or_else(|| SnapshotError::overflow("child range end"))?;
            let ranking = Ranking {
                nodes: &data.nodes,
                names: &data.names,
                refs: &data.file_refs,
            };
            data.children
                .get_mut(start..end)
                .ok_or_else(|| SnapshotError::invalid("invalid child sort range"))?
                .sort_unstable_by(|left, right| ranking.compare(*left, *right, Metric::TotalAlloc));
        }
        self.cursor = self
            .cursor
            .checked_add(1)
            .ok_or_else(|| SnapshotError::overflow("directory sort cursor"))?;
        Ok(directory)
    }

    fn collect(&mut self, data: &mut SnapshotStorage) -> Result<(), SnapshotError> {
        let Some(node) = data.nodes.get(self.cursor) else {
            let count = self.groups.len();
            data.ext_stats.try_reserve_exact(count)?;
            self.groups.prepare_ordering()?;
            self.stage = Stage::HeapBuild;
            return Ok(());
        };
        let id = NodeId::from_raw(
            u32::try_from(self.cursor).map_err(|_| SnapshotError::overflow("file id"))?,
        );
        self.cursor = self
            .cursor
            .checked_add(1)
            .ok_or_else(|| SnapshotError::overflow("file cursor"))?;
        if !node.is_primary_file() {
            return Ok(());
        }
        let ranking = Ranking {
            nodes: &data.nodes,
            names: &data.names,
            refs: &data.file_refs,
        };
        ranking.select_top(&mut data.top_files, id, TOP_FILES)?;
        let suffix = Extension::from_name(
            ranking
                .name(id)
                .ok_or_else(|| SnapshotError::invalid("missing file name"))?,
        );
        self.groups.add(suffix, node.size, node.alloc)?;
        Ok(())
    }

    pub(crate) fn step(&mut self, snapshot: &mut Snapshot) -> Result<bool, SnapshotError> {
        for _ in 0..ITEMS_PER_STEP {
            match self.stage {
                Stage::SortChildren => {
                    if self.sort_children(snapshot.data_mut())? {
                        return Ok(false);
                    }
                }
                Stage::Collect => self.collect(snapshot.data_mut())?,
                Stage::HeapBuild => {
                    if !self.groups.build_next() {
                        self.stage = Stage::HeapDrain;
                    }
                }
                Stage::HeapDrain => {
                    if let Some(group) = self.groups.pop_ordered() {
                        snapshot.data_mut().ext_stats.push(group);
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
