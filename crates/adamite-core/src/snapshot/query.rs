//! Fallible subtree ranking and primary-file summaries without I/O.

use super::{
    ExtensionStat, FileKind, KindStat, Node, NodeId, Snapshot, SnapshotError,
    indexes::ExtensionGroups,
    rules::{Extension, Ranking},
};
use snafu::Snafu;
use std::collections::TryReserveError;

/// Invalid query input, allocation failure or a snapshot calculation failure.
#[must_use]
#[derive(Debug, Snafu)]
#[non_exhaustive]
pub enum QueryError {
    /// The subtree root does not exist in this snapshot.
    #[snafu(display("query node {} does not exist in the snapshot", node.raw()))]
    InvalidNode {
        /// Identifier supplied as the query scope.
        node: NodeId,
    },
    /// The category name is not one of the names in [`FileKind::ALL`].
    #[snafu(display("unknown file kind"))]
    InvalidKind,
    /// Fallible query storage reservation failed.
    #[snafu(display("query allocation failed: {source}"))]
    Allocation {
        /// Underlying allocation failure.
        source: TryReserveError,
    },
    /// A shared snapshot operation failed.
    #[snafu(display("snapshot query failed: {source}"))]
    Snapshot {
        /// Underlying snapshot allocation, overflow or invariant failure.
        source: SnapshotError,
    },
}

impl From<TryReserveError> for QueryError {
    fn from(source: TryReserveError) -> Self {
        Self::Allocation { source }
    }
}

impl From<SnapshotError> for QueryError {
    fn from(source: SnapshotError) -> Self {
        Self::Snapshot { source }
    }
}

/// Selects primary files by suffix or by the category inferred from that suffix.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum FileFilter {
    /// Matches every primary file.
    #[default]
    All,
    /// Matches any ASCII-lowercased suffix in the list, excluding the leading dot.
    ///
    /// Use [`Self::extensions`] to normalize user input. An empty suffix matches names without a usable extension; an empty list matches nothing.
    Extensions(Vec<String>),
    /// Matches files belonging to the supplied category.
    Kind(FileKind),
}

impl FileFilter {
    /// Drops one leading dot from each suffix and folds ASCII letters to lowercase.
    ///
    /// An empty suffix matches the same names as the empty group in [`Snapshot::ext_stats`]. Entries are alternatives, and repeated suffixes are removed.
    ///
    /// # Errors
    ///
    /// Returns an allocation error if the suffix list or its keys cannot be reserved.
    pub fn extensions(
        extensions: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> Result<Self, QueryError> {
        let mut keys = Vec::new();
        for extension in extensions {
            let suffix = extension.as_ref();
            let suffix = suffix.strip_prefix('.').unwrap_or(suffix);
            let key = Extension::from_suffix(suffix).folded()?;
            keys.try_reserve(1)?;
            keys.push(key);
        }
        keys.sort_unstable();
        keys.dedup();
        Ok(Self::Extensions(keys))
    }

    fn matches(&self, name: &str) -> bool {
        match self {
            Self::All => true,
            Self::Extensions(keys) => {
                let suffix = Extension::from_name(name);
                keys.iter().any(|key| suffix.compare_folded(key).is_eq())
            }
            Self::Kind(kind) => FileKind::from_extension(&Extension::from_name(name)) == *kind,
        }
    }
}

impl Snapshot {
    /// Returns up to `n` matching primary files beneath `under`, including a file scope itself.
    ///
    /// Results use descending self allocation, then the same UTF-16 name, file reference and identifier ordering as [`Self::top_files`]. Traversal is iterative and selection retains at most `n` identifiers. A zero limit returns an empty list.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidNode`] for an unknown scope, or an allocation or snapshot error if traversal or ranking fails.
    pub fn rank_files(
        &self,
        under: NodeId,
        filter: &FileFilter,
        n: usize,
    ) -> Result<Vec<NodeId>, QueryError> {
        self.check_scope(under)?;
        let mut result = Vec::new();
        if n == 0 {
            return Ok(result);
        }
        let ranking = Ranking {
            nodes: &self.data().nodes,
            names: &self.data().names,
            refs: &self.data().file_refs,
        };
        self.visit_primary_files(under, |id, _, name| {
            if filter.matches(name) {
                ranking.select_top(&mut result, id, n)?;
            }
            Ok(())
        })?;
        Ok(result)
    }

    /// Groups primary files beneath `under` by the shared folded filename extension.
    ///
    /// A file scope includes that file when it is primary. Results use descending self allocation, then ascending suffix, as in [`Self::ext_stats`]. The root scope returns a fallibly allocated copy of that stored index.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidNode`] for an unknown scope, or an allocation, overflow or snapshot error while accumulating summaries.
    pub fn extension_stats_under(&self, under: NodeId) -> Result<Vec<ExtensionStat>, QueryError> {
        self.check_scope(under)?;
        if under == NodeId::ROOT {
            let mut result = Vec::new();
            result.try_reserve_exact(self.ext_stats().len())?;
            for group in self.ext_stats() {
                result.push(group.try_clone()?);
            }
            return Ok(result);
        }
        let mut groups = ExtensionGroups::default();
        self.visit_primary_files(under, |_, node, name| {
            groups.add(Extension::from_name(name), node.size, node.alloc)?;
            Ok(())
        })?;
        Ok(groups.into_sorted()?)
    }

    /// Groups subtree extension summaries into file categories, omitting empty categories.
    ///
    /// Results use descending allocation, then the category order in [`FileKind::ALL`]. A file scope includes that file when it is primary.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidNode`] for an unknown scope, or an allocation, overflow or snapshot error while accumulating summaries.
    pub fn kind_stats(&self, under: NodeId) -> Result<Vec<KindStat>, QueryError> {
        let extensions = self.extension_stats_under(under)?;
        let mut result = Vec::new();
        result.try_reserve_exact(FileKind::ALL.len())?;
        for kind in FileKind::ALL {
            result.push(KindStat {
                kind,
                count: 0,
                total_size: 0,
                total_alloc: 0,
            });
        }
        for extension in extensions {
            let kind = FileKind::from_extension(&Extension::from_suffix(extension.extension()));
            let group = result
                .iter_mut()
                .find(|group| group.kind == kind)
                .ok_or_else(|| SnapshotError::invalid("missing kind accumulator"))?;
            group.add(&extension)?;
        }
        result.retain(|group| group.count != 0);
        result.sort_unstable_by(|left, right| {
            right
                .total_alloc
                .cmp(&left.total_alloc)
                .then_with(|| left.kind.cmp(&right.kind))
        });
        Ok(result)
    }

    fn check_scope(&self, under: NodeId) -> Result<(), QueryError> {
        self.node(under)
            .ok_or(QueryError::InvalidNode { node: under })?;
        Ok(())
    }

    fn visit_primary_files(
        &self,
        under: NodeId,
        mut visit: impl FnMut(NodeId, &Node, &str) -> Result<(), QueryError>,
    ) -> Result<(), QueryError> {
        let mut pending = Vec::new();
        pending.try_reserve(1)?;
        pending.push(under);
        while let Some(id) = pending.pop() {
            let node = self
                .node(id)
                .ok_or_else(|| SnapshotError::invalid("missing query node"))?;
            if node.node.is_primary_file() {
                let name = self
                    .name(id)
                    .ok_or_else(|| SnapshotError::invalid("missing query name"))?;
                visit(id, node.node, name)?;
            }
            let children = self
                .children(id)
                .ok_or_else(|| SnapshotError::invalid("missing query children"))?;
            pending.try_reserve(children.len())?;
            pending.extend_from_slice(children);
        }
        Ok(())
    }
}
