//! Bounded breadth-first traversal and reverse accumulation over immutable CSR edges.

use crate::{Node, NodeFlags, NodeId, Snapshot, SnapshotError, snapshot::SnapshotStorage};

/// Maximum nodes or CSR edges processed before yielding to the caller.
const ITEMS_PER_STEP: usize = 256;

#[derive(Debug)]
enum Stage {
    BreadthFirst,
    Initialize,
    Accumulate,
    Done,
}

#[derive(Debug)]
pub(crate) struct Aggregator {
    order: Vec<NodeId>,
    seen: Vec<u8>,
    cursor: usize,
    child: usize,
    stage: Stage,
}

pub(super) fn overflow(field: &'static str) -> SnapshotError {
    SnapshotError::Overflow { field }
}
pub(super) fn invalid(reason: &'static str) -> SnapshotError {
    SnapshotError::InvalidTree { reason }
}
pub(super) fn index(id: NodeId) -> Result<usize, SnapshotError> {
    usize::try_from(id.raw()).map_err(|_| overflow("node index"))
}
pub(super) fn has(node: &Node, flag: NodeFlags) -> bool {
    node.flags & flag.raw() != 0
}
pub(super) fn primary_file(node: &Node) -> bool {
    !has(node, NodeFlags::DIRECTORY)
        && !has(node, NodeFlags::SYNTHETIC)
        && !has(node, NodeFlags::HARDLINK_DUP)
}

impl Aggregator {
    pub(crate) fn new(snapshot: &Snapshot) -> Result<Self, SnapshotError> {
        if snapshot.is_empty() {
            return Err(invalid("missing root"));
        }
        let mut order = Vec::new();
        order
            .try_reserve_exact(snapshot.len())
            .map_err(|source| SnapshotError::Allocation { source })?;
        order.push(NodeId::ROOT);
        let mut seen = Vec::new();
        seen.try_reserve_exact(snapshot.len())
            .map_err(|source| SnapshotError::Allocation { source })?;
        seen.resize(snapshot.len(), 0);
        *seen
            .first_mut()
            .ok_or_else(|| invalid("missing root mark"))? = 1;
        Ok(Self {
            order,
            seen,
            cursor: 0,
            child: 0,
            stage: Stage::BreadthFirst,
        })
    }

    fn breadth_first(&mut self, data: &SnapshotStorage) -> Result<(), SnapshotError> {
        let Some(id) = self.order.get(self.cursor).copied() else {
            if self.order.len() != data.nodes.len() {
                return Err(invalid("unreachable nodes"));
            }
            self.stage = Stage::Initialize;
            self.cursor = 0;
            return Ok(());
        };
        let node = data
            .nodes
            .get(index(id)?)
            .ok_or_else(|| invalid("missing BFS node"))?;
        if self.child < usize::try_from(node.child_len).map_err(|_| overflow("child count"))? {
            let offset = usize::try_from(node.child_start)
                .map_err(|_| overflow("child start"))?
                .checked_add(self.child)
                .ok_or_else(|| overflow("child cursor"))?;
            let child = *data
                .children
                .get(offset)
                .ok_or_else(|| invalid("missing CSR edge"))?;
            let child_index = index(child)?;
            let mark = self
                .seen
                .get_mut(child_index)
                .ok_or_else(|| invalid("child outside arena"))?;
            if *mark != 0 {
                return Err(invalid("repeated or cyclic child"));
            }
            if data
                .nodes
                .get(child_index)
                .ok_or_else(|| invalid("missing child"))?
                .parent
                != id.raw()
            {
                return Err(invalid("CSR parent mismatch"));
            }
            *mark = 1;
            self.order.push(child);
            self.child = self
                .child
                .checked_add(1)
                .ok_or_else(|| overflow("BFS child cursor"))?;
        } else {
            self.cursor = self
                .cursor
                .checked_add(1)
                .ok_or_else(|| overflow("BFS cursor"))?;
            self.child = 0;
        }
        Ok(())
    }

    fn initialize(&mut self, data: &mut SnapshotStorage) -> Result<(), SnapshotError> {
        if let Some(node) = data.nodes.get_mut(self.cursor) {
            node.total_size = node.size;
            node.total_alloc = node.alloc;
            node.file_count = u32::from(primary_file(node));
            self.cursor = self
                .cursor
                .checked_add(1)
                .ok_or_else(|| overflow("initialization cursor"))?;
        } else {
            self.stage = Stage::Accumulate;
            self.cursor = self.order.len();
        }
        Ok(())
    }

    fn accumulate(&mut self, data: &mut SnapshotStorage) -> Result<(), SnapshotError> {
        let Some(cursor) = self.cursor.checked_sub(1) else {
            self.stage = Stage::Done;
            return Ok(());
        };
        self.cursor = cursor;
        let id = *self
            .order
            .get(cursor)
            .ok_or_else(|| invalid("missing reverse BFS node"))?;
        let node = *data
            .nodes
            .get(index(id)?)
            .ok_or_else(|| invalid("missing accumulation node"))?;
        // Duplicate links retain their own sizes and totals, but never enter parent totals.
        if id == NodeId::ROOT || has(&node, NodeFlags::HARDLINK_DUP) {
            return Ok(());
        }
        let parent = data
            .nodes
            .get_mut(index(NodeId::from_raw(node.parent))?)
            .ok_or_else(|| invalid("missing accumulation parent"))?;
        let size = parent
            .total_size
            .checked_add(node.total_size)
            .ok_or_else(|| overflow("subtree logical bytes"))?;
        let alloc = parent
            .total_alloc
            .checked_add(node.total_alloc)
            .ok_or_else(|| overflow("subtree allocation"))?;
        let files = parent
            .file_count
            .checked_add(node.file_count)
            .ok_or_else(|| overflow("subtree file count"))?;
        parent.total_size = size;
        parent.total_alloc = alloc;
        parent.file_count = files;
        Ok(())
    }

    pub(crate) fn step(&mut self, snapshot: &mut Snapshot) -> Result<bool, SnapshotError> {
        for _ in 0..ITEMS_PER_STEP {
            match self.stage {
                Stage::BreadthFirst => self.breadth_first(snapshot.data())?,
                Stage::Initialize => self.initialize(snapshot.data_mut())?,
                Stage::Accumulate => self.accumulate(snapshot.data_mut())?,
                Stage::Done => return Ok(true),
            }
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests;
