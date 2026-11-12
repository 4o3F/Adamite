//! Budgeted tree summaries over size-ordered snapshot children.

use super::{NodeId, QueryError, Snapshot, SnapshotError};
use std::{cmp::Reverse, collections::BinaryHeap};

/// A tree-ordered selection of nodes and collapsed sibling groups.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    /// Pre-order rows, including the requested root at depth zero.
    pub rows: Vec<SummaryRow>,
    /// Whether the row budget prevented any candidate from being expanded.
    /// Share-threshold collapsing alone does not set this flag.
    pub truncated: bool,
}

/// A snapshot node or the remaining direct children of an emitted directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SummaryRow {
    /// An individually selected node.
    Node {
        /// Identifier in the queried snapshot.
        id: NodeId,
        /// Distance from the requested root.
        depth: usize,
    },
    /// A suffix of siblings in stored child order, including their subtrees.
    Collapsed {
        /// Directory containing the omitted children.
        parent: NodeId,
        /// Distance of these children from the requested root.
        depth: usize,
        /// Number of omitted direct children.
        count: usize,
        /// Sum of their subtree allocations in bytes.
        alloc: u64,
        /// Sum of their subtree logical sizes in bytes.
        size: u64,
    },
}

/// One next child per expandable sibling group; ties prefer the lower NodeId.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Candidate {
    alloc: u64,
    id: Reverse<NodeId>,
    parent: NodeId,
    offset: usize,
}

impl Snapshot {
    /// Selects a tree summary using a lazy maximum-allocation candidate heap.
    ///
    /// Every row counts toward `budget`. Shares use the requested root's subtree
    /// allocation and basis points (10,000 means 100%). A child below the threshold
    /// collapses with its later siblings. A zero-allocation root ignores the threshold.
    ///
    /// Each emitted directory with remaining children reserves one collapsed row.
    /// A candidate replaces that row only if the node, its own child group and the
    /// remaining sibling group all fit. Candidates that do not fit remain collapsed;
    /// other heap candidates are still considered. Budget one returns only the root.
    /// Selection ties across directories prefer the lower NodeId. Output follows
    /// pre-order and stored child order. Memory is O(budget); inserting selected rows
    /// takes O(budget squared) time, plus a scan of each opened directory's children.
    ///
    /// # Errors
    /// Returns [`QueryError::InvalidArgument`] for budget zero, an invalid-node error
    /// for an unknown root, or an allocation, arithmetic or snapshot invariant error.
    pub fn summarize(
        &self,
        root: NodeId,
        budget: usize,
        min_share_bp: u32,
    ) -> Result<Summary, QueryError> {
        if budget == 0 {
            return Err(QueryError::InvalidArgument {
                reason: "summary budget must be at least one",
            });
        }
        let denominator = self
            .node(root)
            .ok_or(QueryError::InvalidNode { node: root })?
            .total_alloc();
        let threshold = u128::from(denominator)
            .checked_mul(u128::from(min_share_bp))
            .ok_or_else(|| SnapshotError::overflow("summary share threshold"))?;
        let mut summary = Summary {
            rows: Vec::new(),
            truncated: false,
        };
        summary.rows.try_reserve(1)?;
        summary.rows.push(SummaryRow::Node { id: root, depth: 0 });
        if budget == 1 {
            summary.truncated = !self
                .children(root)
                .ok_or_else(|| SnapshotError::invalid("missing summary children"))?
                .is_empty();
            return Ok(summary);
        }
        let mut heap = BinaryHeap::new();
        if let Some(group) = self.summary_group(root, 1)? {
            summary.rows.try_reserve(1)?;
            summary.rows.push(group);
            self.summary_candidate(&mut heap, root, 0)?;
        }
        while let Some(candidate) = heap.pop() {
            let id = candidate.id.0;
            let share = u128::from(candidate.alloc)
                .checked_mul(10_000)
                .ok_or_else(|| SnapshotError::overflow("summary share"))?;
            if denominator != 0 && share < threshold {
                continue;
            }
            let position = summary
                .rows
                .iter()
                .position(|row| matches!(row, SummaryRow::Collapsed { parent, .. } if *parent == candidate.parent))
                .ok_or_else(|| SnapshotError::invalid("missing summary sibling group"))?;
            let tail = *summary
                .rows
                .get(position)
                .ok_or_else(|| SnapshotError::invalid("missing summary row"))?;
            let SummaryRow::Collapsed {
                depth,
                count,
                alloc,
                size,
                ..
            } = tail
            else {
                return Err(SnapshotError::invalid("invalid summary sibling group").into());
            };
            let node = self.node(id).ok_or(QueryError::InvalidNode { node: id })?;
            let next_depth = depth
                .checked_add(1)
                .ok_or_else(|| SnapshotError::overflow("summary depth"))?;
            let has_children = !self
                .children(id)
                .ok_or_else(|| SnapshotError::invalid("missing summary children"))?
                .is_empty();
            let remaining = count
                .checked_sub(1)
                .ok_or_else(|| SnapshotError::overflow("summary child count"))?;
            let additional = usize::from(has_children)
                .checked_add(usize::from(remaining != 0))
                .ok_or_else(|| SnapshotError::overflow("summary row count"))?;
            let needed = summary
                .rows
                .len()
                .checked_add(additional)
                .ok_or_else(|| SnapshotError::overflow("summary row count"))?;
            if needed > budget {
                summary.truncated = true;
                continue;
            }
            summary.rows.try_reserve(additional)?;
            *summary
                .rows
                .get_mut(position)
                .ok_or_else(|| SnapshotError::invalid("missing summary row"))? =
                SummaryRow::Node { id, depth };
            let after = position
                .checked_add(1)
                .ok_or_else(|| SnapshotError::overflow("summary row position"))?;
            if remaining != 0 {
                summary.rows.insert(
                    after,
                    SummaryRow::Collapsed {
                        parent: candidate.parent,
                        depth,
                        count: remaining,
                        alloc: alloc
                            .checked_sub(node.total_alloc())
                            .ok_or_else(|| SnapshotError::overflow("summary allocation"))?,
                        size: size
                            .checked_sub(node.total_size())
                            .ok_or_else(|| SnapshotError::overflow("summary logical size"))?,
                    },
                );
                let offset = candidate
                    .offset
                    .checked_add(1)
                    .ok_or_else(|| SnapshotError::overflow("summary child offset"))?;
                self.summary_candidate(&mut heap, candidate.parent, offset)?;
            }
            if let Some(group) = self.summary_group(id, next_depth)? {
                summary.rows.insert(after, group);
                self.summary_candidate(&mut heap, id, 0)?;
            }
        }
        Ok(summary)
    }

    fn summary_group(
        &self,
        parent: NodeId,
        depth: usize,
    ) -> Result<Option<SummaryRow>, QueryError> {
        let children = self
            .children(parent)
            .ok_or_else(|| SnapshotError::invalid("missing summary children"))?;
        if children.is_empty() {
            return Ok(None);
        }
        let mut alloc = 0u64;
        let mut size = 0u64;
        for &id in children {
            let node = self.node(id).ok_or(QueryError::InvalidNode { node: id })?;
            alloc = alloc
                .checked_add(node.total_alloc())
                .ok_or_else(|| SnapshotError::overflow("summary allocation"))?;
            size = size
                .checked_add(node.total_size())
                .ok_or_else(|| SnapshotError::overflow("summary logical size"))?;
        }
        Ok(Some(SummaryRow::Collapsed {
            parent,
            depth,
            count: children.len(),
            alloc,
            size,
        }))
    }

    fn summary_candidate(
        &self,
        heap: &mut BinaryHeap<Candidate>,
        parent: NodeId,
        offset: usize,
    ) -> Result<(), QueryError> {
        let id = *self
            .children(parent)
            .and_then(|children| children.get(offset))
            .ok_or_else(|| SnapshotError::invalid("missing summary candidate"))?;
        let alloc = self
            .node(id)
            .ok_or(QueryError::InvalidNode { node: id })?
            .total_alloc();
        heap.try_reserve(1)?;
        heap.push(Candidate {
            alloc,
            id: Reverse(id),
            parent,
            offset,
        });
        Ok(())
    }
}
