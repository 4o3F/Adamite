use super::PersistError;
use crate::snapshot::{
    Node, NodeFlags, NodeId,
    rules::{Extension, Metric, Ranking},
    storage::SnapshotStorage,
};
use adamite_ntfs::value::standard_information::FileAttributes;

pub(super) struct Validator<'a> {
    data: &'a SnapshotStorage,
}

struct ExtensionTotal<'a> {
    extension: &'a str,
    count: u64,
    size: u64,
    alloc: u64,
    stored: usize,
}

impl<'a> Validator<'a> {
    pub(super) fn new(data: &'a SnapshotStorage) -> Self {
        Self { data }
    }

    pub(super) fn validate(&self) -> Result<(), PersistError> {
        self.columns()?;
        self.names()?;
        self.root()?;
        self.arena_order()?;
        self.flags()?;
        self.csr()?;
        self.connectivity()?;
        self.aggregates()?;
        self.child_order()?;
        self.top_files()?;
        self.extensions()
    }

    fn invalid(&self, index: usize, reason: &'static str) -> PersistError {
        PersistError::InvalidTree {
            reason,
            node: u32::try_from(index).unwrap_or(u32::MAX),
        }
    }

    fn index(raw: u32) -> Result<usize, PersistError> {
        usize::try_from(raw).map_err(|_| PersistError::Overflow {
            operation: "node index",
        })
    }

    fn id(index: usize) -> Result<NodeId, PersistError> {
        u32::try_from(index)
            .map(NodeId::from_raw)
            .map_err(|_| PersistError::Overflow {
                operation: "node id",
            })
    }

    fn node(&self, index: usize) -> Result<&'a Node, PersistError> {
        self.data
            .nodes
            .get(index)
            .ok_or_else(|| self.invalid(index, "node outside arena"))
    }

    fn column<T: Copy>(&self, column: &[T], index: usize) -> Result<T, PersistError> {
        column
            .get(index)
            .copied()
            .ok_or_else(|| self.invalid(index, "missing cold column value"))
    }

    fn children(&self, index: usize) -> Result<&'a [NodeId], PersistError> {
        let node = self.node(index)?;
        let start = Self::index(node.child_start)?;
        let end = start
            .checked_add(Self::index(node.child_len)?)
            .ok_or_else(|| self.invalid(index, "child range overflow"))?;
        self.data
            .children
            .get(start..end)
            .ok_or_else(|| self.invalid(index, "child range outside CSR"))
    }

    fn ranking(&self) -> Ranking<'a> {
        Ranking {
            nodes: &self.data.nodes,
            names: &self.data.names,
            refs: &self.data.file_refs,
        }
    }

    fn name(&self, index: usize) -> Result<&'a str, PersistError> {
        let node = self.node(index)?;
        let start = Self::index(node.name_off)?;
        let end = start
            .checked_add(usize::from(node.name_len))
            .ok_or_else(|| self.invalid(index, "name range overflow"))?;
        self.data
            .names
            .get(start..end)
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .ok_or_else(|| self.invalid(index, "invalid UTF-8 name range"))
    }

    fn columns(&self) -> Result<(), PersistError> {
        let n = self.data.nodes.len();
        if n == 0 || u32::try_from(n).is_err() {
            return Err(self.invalid(0, "node count outside 1..=u32::MAX"));
        }
        if [
            self.data.ads_size.len(),
            self.data.file_refs.len(),
            self.data.attrs.len(),
            self.data.nlink.len(),
            self.data.reparse_tags.len(),
        ]
        .into_iter()
        .any(|len| len != n)
        {
            return Err(self.invalid(0, "cold column length differs from node count"));
        }
        if u32::try_from(self.data.names.len()).is_err() {
            return Err(self.invalid(0, "name pool exceeds u32"));
        }
        Ok(())
    }

    fn names(&self) -> Result<(), PersistError> {
        let mut offset = 0usize;
        for (index, node) in self.data.nodes.iter().enumerate() {
            if Self::index(node.name_off)? != offset {
                return Err(self.invalid(index, "names are not tightly packed in arena order"));
            }
            self.name(index)?;
            offset = offset
                .checked_add(usize::from(node.name_len))
                .ok_or_else(|| self.invalid(index, "name range overflow"))?;
        }
        if offset != self.data.names.len() {
            return Err(self.invalid(0, "names do not cover pool"));
        }
        Ok(())
    }

    fn root(&self) -> Result<(), PersistError> {
        let root = self.node(0)?;
        if root.parent != 0
            || self.name(0)? != "/"
            || !root.has(NodeFlags::DIRECTORY)
            || root.has(NodeFlags::SYNTHETIC)
            || root.has(NodeFlags::HARDLINK_DUP)
            || !self
                .column(&self.data.file_refs, 0)?
                .is_some_and(|reference| reference.record() == 5)
        {
            return Err(self.invalid(0, "invalid root identity, parent, name or flags"));
        }
        Ok(())
    }

    fn synthetic(&self, index: usize) -> Result<(), PersistError> {
        let node = self.node(index)?;
        if node.flags != NodeFlags::DIRECTORY.raw() | NodeFlags::SYNTHETIC.raw()
            || node.parent != 0
            || node.size != 0
            || node.alloc != 0
            || node.mtime != 0
            || self.column(&self.data.ads_size, index)? != 0
            || self.column(&self.data.attrs, index)? != 0
            || self.column(&self.data.nlink, index)? != 0
            || self.column(&self.data.file_refs, index)?.is_some()
            || self.column(&self.data.reparse_tags, index)?.is_some()
        {
            return Err(self.invalid(index, "invalid synthetic directory metadata"));
        }
        Ok(())
    }

    fn same_record(&self, primary: usize, index: usize) -> Result<(), PersistError> {
        let first = self.node(primary)?;
        let node = self.node(index)?;
        let mask = !NodeFlags::HARDLINK_DUP.raw();
        if node.size != first.size
            || node.alloc != first.alloc
            || node.mtime != first.mtime
            || node.flags & mask != first.flags & mask
            || self.column(&self.data.ads_size, index)?
                != self.column(&self.data.ads_size, primary)?
            || self.column(&self.data.attrs, index)? != self.column(&self.data.attrs, primary)?
            || self.column(&self.data.nlink, index)? != self.column(&self.data.nlink, primary)?
            || self.column(&self.data.reparse_tags, index)?
                != self.column(&self.data.reparse_tags, primary)?
            || self.column(&self.data.file_refs, index)?
                != self.column(&self.data.file_refs, primary)?
        {
            return Err(self.invalid(index, "hard-link group metadata differs"));
        }
        Ok(())
    }

    fn arena_order(&self) -> Result<(), PersistError> {
        let mut synthetic_rank = 0;
        let mut primary = None;
        let mut previous_record = None;
        for (index, node) in self.data.nodes.iter().enumerate().skip(1) {
            if node.has(NodeFlags::SYNTHETIC) {
                if primary.is_some() {
                    return Err(self.invalid(index, "synthetic directory follows record nodes"));
                }
                let rank = match self.name(index)? {
                    "<NTFS metadata>" => 1,
                    "<orphaned>" => 2,
                    _ => return Err(self.invalid(index, "unknown synthetic directory name")),
                };
                if rank <= synthetic_rank {
                    return Err(
                        self.invalid(index, "synthetic directories repeated or out of order")
                    );
                }
                synthetic_rank = rank;
                self.synthetic(index)?;
                continue;
            }
            let reference = self
                .column(&self.data.file_refs, index)?
                .ok_or_else(|| self.invalid(index, "record node has no file reference"))?;
            let record = reference.record();
            if record == 5 || previous_record.is_some_and(|previous| record < previous) {
                return Err(self.invalid(
                    index,
                    "record groups are not ascending or include root record",
                ));
            }
            if previous_record == Some(record) {
                if !node.has(NodeFlags::HARDLINK_DUP) {
                    return Err(self.invalid(index, "hard-link group has multiple primary nodes"));
                }
                self.same_record(
                    primary.ok_or_else(|| self.invalid(index, "missing group primary"))?,
                    index,
                )?;
            } else {
                if node.has(NodeFlags::HARDLINK_DUP) {
                    return Err(self.invalid(index, "hard-link group does not start with primary"));
                }
                primary = Some(index);
                previous_record = Some(record);
            }
        }
        Ok(())
    }

    fn flags(&self) -> Result<(), PersistError> {
        let known = NodeFlags::DIRECTORY.raw()
            | NodeFlags::REPARSE.raw()
            | NodeFlags::SPARSE.raw()
            | NodeFlags::COMPRESSED.raw()
            | NodeFlags::HIDDEN.raw()
            | NodeFlags::SYSTEM.raw()
            | NodeFlags::HARDLINK_DUP.raw()
            | NodeFlags::SYNTHETIC.raw();
        for (index, node) in self.data.nodes.iter().enumerate() {
            if node.flags & !known != 0 {
                return Err(self.invalid(index, "unknown node flag bits"));
            }
            let attrs = FileAttributes::from_raw(self.column(&self.data.attrs, index)?);
            if node.has(NodeFlags::HIDDEN) != attrs.contains(FileAttributes::HIDDEN)
                || node.has(NodeFlags::SYSTEM) != attrs.contains(FileAttributes::SYSTEM)
            {
                return Err(self.invalid(index, "hidden or system flag differs from attributes"));
            }
            let reparse = attrs.contains(FileAttributes::REPARSE_POINT)
                || self.column(&self.data.reparse_tags, index)?.is_some();
            if node.has(NodeFlags::REPARSE) != reparse {
                return Err(self.invalid(index, "reparse flag differs from attributes and tag"));
            }
            for (attribute, flag) in [
                (FileAttributes::DIRECTORY, NodeFlags::DIRECTORY),
                (FileAttributes::SPARSE_FILE, NodeFlags::SPARSE),
                (FileAttributes::COMPRESSED, NodeFlags::COMPRESSED),
            ] {
                if attrs.contains(attribute) && !node.has(flag) {
                    return Err(self.invalid(index, "attribute requires missing node flag"));
                }
            }
        }
        Ok(())
    }

    fn marks(&self) -> Result<Vec<u8>, PersistError> {
        let mut marks = Vec::new();
        marks.try_reserve_exact(self.data.nodes.len())?;
        marks.resize(self.data.nodes.len(), 0);
        Ok(marks)
    }

    fn csr(&self) -> Result<(), PersistError> {
        let expected = self
            .data
            .nodes
            .len()
            .checked_sub(1)
            .ok_or_else(|| self.invalid(0, "missing root"))?;
        if self.data.children.len() != expected {
            return Err(self.invalid(0, "CSR edge count is not node count minus one"));
        }
        let mut seen = self.marks()?;
        let mut offset = 0usize;
        for (index, node) in self.data.nodes.iter().enumerate() {
            if Self::index(node.child_start)? != offset {
                return Err(self.invalid(index, "CSR ranges do not partition edges in arena order"));
            }
            let children = self.children(index)?;
            if !children.is_empty()
                && (!node.has(NodeFlags::DIRECTORY) || node.has(NodeFlags::HARDLINK_DUP))
            {
                return Err(self.invalid(index, "non-directory or duplicate node has children"));
            }
            for child in children {
                let child_index = Self::index(child.raw())?;
                let mark = seen
                    .get_mut(child_index)
                    .ok_or_else(|| self.invalid(child_index, "child outside arena"))?;
                if child_index == 0 || *mark != 0 {
                    return Err(self.invalid(child_index, "root or repeated child in CSR"));
                }
                *mark = 1;
                if self.node(child_index)?.parent != Self::id(index)?.raw() {
                    return Err(self.invalid(child_index, "CSR parent mismatch"));
                }
            }
            offset = offset
                .checked_add(children.len())
                .ok_or_else(|| self.invalid(index, "CSR range overflow"))?;
        }
        if offset != self.data.children.len() || seen.iter().skip(1).any(|&mark| mark != 1) {
            return Err(self.invalid(0, "CSR does not contain every non-root node exactly once"));
        }
        Ok(())
    }

    fn connectivity(&self) -> Result<(), PersistError> {
        let mut seen = self.marks()?;
        let mut order = Vec::new();
        order.try_reserve_exact(self.data.nodes.len())?;
        order.push(NodeId::ROOT);
        *seen
            .first_mut()
            .ok_or_else(|| self.invalid(0, "missing root mark"))? = 1;
        let mut cursor = 0usize;
        while let Some(id) = order.get(cursor).copied() {
            for child in self.children(Self::index(id.raw())?)? {
                let index = Self::index(child.raw())?;
                let mark = seen
                    .get_mut(index)
                    .ok_or_else(|| self.invalid(index, "child outside arena"))?;
                if *mark != 0 {
                    return Err(self.invalid(index, "repeated or cyclic child"));
                }
                *mark = 1;
                order.push(*child);
            }
            cursor = cursor.checked_add(1).ok_or(PersistError::Overflow {
                operation: "traversal cursor",
            })?;
        }
        if order.len() != self.data.nodes.len() {
            return Err(self.invalid(0, "nodes unreachable from root"));
        }
        Ok(())
    }

    fn aggregates(&self) -> Result<(), PersistError> {
        for (index, node) in self.data.nodes.iter().enumerate() {
            let mut size = node.size;
            let mut alloc = node.alloc;
            let mut files = u32::from(node.is_primary_file());
            for child in self.children(index)? {
                let child = self.node(Self::index(child.raw())?)?;
                if child.has(NodeFlags::HARDLINK_DUP) {
                    continue;
                }
                size = size
                    .checked_add(child.total_size)
                    .ok_or_else(|| self.invalid(index, "subtree logical bytes overflow"))?;
                alloc = alloc
                    .checked_add(child.total_alloc)
                    .ok_or_else(|| self.invalid(index, "subtree allocation overflow"))?;
                files = files
                    .checked_add(child.file_count)
                    .ok_or_else(|| self.invalid(index, "subtree file count overflow"))?;
            }
            if (size, alloc, files) != (node.total_size, node.total_alloc, node.file_count) {
                return Err(self.invalid(
                    index,
                    "subtree totals differ from self and primary children",
                ));
            }
        }
        Ok(())
    }

    fn child_order(&self) -> Result<(), PersistError> {
        let ranking = self.ranking();
        for index in 0..self.data.nodes.len() {
            for pair in self.children(index)?.windows(2) {
                if let [left, right] = pair
                    && ranking.compare(*left, *right, Metric::TotalAlloc).is_gt()
                {
                    return Err(
                        self.invalid(index, "children are not in total-allocation ranking order")
                    );
                }
            }
        }
        Ok(())
    }

    fn top_files(&self) -> Result<(), PersistError> {
        let ranking = self.ranking();
        let mut top = Vec::new();
        for (index, node) in self.data.nodes.iter().enumerate() {
            if node.is_primary_file() {
                ranking
                    .select_top(&mut top, Self::id(index)?)
                    .map_err(PersistError::from_snapshot)?;
            }
        }
        if top != self.data.top_files {
            return Err(PersistError::InvalidIndex {
                reason: "top files differ from bounded primary-file selection",
            });
        }
        Ok(())
    }

    fn extensions(&self) -> Result<(), PersistError> {
        let invalid = |reason| PersistError::InvalidIndex { reason };
        for pair in self.data.ext_stats.windows(2) {
            if let [left, right] = pair
                && (left.total_alloc < right.total_alloc
                    || (left.total_alloc == right.total_alloc && left.extension >= right.extension))
            {
                return Err(invalid(
                    "extension summaries are not in allocation and name order",
                ));
            }
        }
        let mut totals = Vec::new();
        totals.try_reserve_exact(self.data.ext_stats.len())?;
        for (index, group) in self.data.ext_stats.iter().enumerate() {
            totals.push(ExtensionTotal {
                extension: &group.extension,
                count: 0,
                size: 0,
                alloc: 0,
                stored: index,
            });
        }
        totals.sort_unstable_by(|left, right| left.extension.cmp(right.extension));
        if totals
            .windows(2)
            .any(|pair| matches!(pair, [left, right] if left.extension == right.extension))
        {
            return Err(invalid("duplicate extension summary"));
        }
        for (index, node) in self.data.nodes.iter().enumerate() {
            if !node.is_primary_file() {
                continue;
            }
            let suffix = Extension::from_name(self.name(index)?);
            let position = totals
                .binary_search_by(|group| suffix.compare_folded(group.extension))
                .map_err(|_| invalid("missing or noncanonical extension summary"))?;
            let group = totals
                .get_mut(position)
                .ok_or_else(|| invalid("missing extension accumulator"))?;
            group.count = group
                .count
                .checked_add(1)
                .ok_or_else(|| invalid("extension count overflow"))?;
            group.size = group
                .size
                .checked_add(node.size)
                .ok_or_else(|| invalid("extension logical bytes overflow"))?;
            group.alloc = group
                .alloc
                .checked_add(node.alloc)
                .ok_or_else(|| invalid("extension allocation overflow"))?;
        }
        for total in totals {
            let stored = self
                .data
                .ext_stats
                .get(total.stored)
                .ok_or_else(|| invalid("missing stored extension summary"))?;
            if total.count == 0
                || (total.count, total.size, total.alloc)
                    != (stored.count, stored.total_size, stored.total_alloc)
            {
                return Err(invalid(
                    "extension summary differs from primary-file totals",
                ));
            }
        }
        Ok(())
    }
}
