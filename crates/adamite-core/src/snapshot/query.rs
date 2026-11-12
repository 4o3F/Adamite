//! Fallible subtree ranking, name search and primary-file summaries without I/O.

use super::{
    ExtensionStat, FileKind, KindStat, NamePattern, Node, NodeId, Snapshot, SnapshotError,
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
    /// A component does not identify a child of the current directory.
    #[snafu(display("path component {component:?} not found in {path:?}"))]
    PathNotFound {
        /// Full path supplied for resolution.
        path: String,
        /// Component that could not be resolved.
        component: String,
    },
    /// A component matches multiple children at the same resolution step.
    #[snafu(display(
        "ambiguous path component {component:?} in {path:?}; candidate NodeIds: {candidates:?}"
    ))]
    AmbiguousPath {
        /// Full path supplied for resolution.
        path: String,
        /// Component with multiple matching children.
        component: String,
        /// Matching direct children in stored child order.
        candidates: Vec<NodeId>,
    },
    /// Path resolution attempted to descend through a file.
    #[snafu(display("not a directory: node {}", node.raw()))]
    NotADirectory {
        /// File encountered before the end of the path.
        node: NodeId,
    },
    /// The subtree root does not exist in this snapshot.
    #[snafu(display("query node {} does not exist in the snapshot", node.raw()))]
    InvalidNode {
        /// Identifier supplied as the query scope.
        node: NodeId,
    },
    /// The category name is not one of the names in [`FileKind::ALL`].
    #[snafu(display("unknown file kind"))]
    InvalidKind,
    /// A name pattern exceeds the supported byte length.
    #[snafu(display("name pattern exceeds the {limit}-byte limit"))]
    PatternTooLong {
        /// Maximum accepted pattern length in bytes.
        limit: usize,
    },
    /// The supplied name pattern is not a valid glob.
    #[snafu(display("invalid name glob: {source}"))]
    InvalidGlob {
        /// Underlying glob parsing failure.
        source: globset::Error,
    },
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

impl From<globset::Error> for QueryError {
    fn from(source: globset::Error) -> Self {
        Self::InvalidGlob { source }
    }
}

/// One page of matching node identifiers and the full subtree match count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchPage {
    items: Vec<NodeId>,
    total_matches: u64,
    offset: usize,
}

impl SearchPage {
    /// Borrows this page's matches in stored-child pre-order.
    #[must_use]
    pub fn items(&self) -> &[NodeId] {
        &self.items
    }

    /// Returns the total number of matches, including those outside this page.
    #[must_use]
    pub const fn total_matches(&self) -> u64 {
        self.total_matches
    }

    /// Returns the next match offset when more matches remain.
    ///
    /// A zero-sized page retains its offset when matches remain.
    #[must_use]
    pub fn next_offset(&self) -> Option<usize> {
        let next = self.offset.checked_add(self.items.len())?;
        (u64::try_from(next).ok()? < self.total_matches).then_some(next)
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
        if n == 0 {
            return Ok(Vec::new());
        }
        let data = self.data();
        let ranking = Ranking {
            nodes: &data.nodes,
            names: &data.names,
            refs: &data.file_refs,
        };
        let name = |id| {
            ranking
                .name(id)
                .ok_or_else(|| SnapshotError::invalid("missing query name"))
        };
        match filter {
            FileFilter::All => self.rank_matching_files(under, &ranking, n, |_| Ok(true)),
            FileFilter::Extensions(keys) => {
                let keys = keys.as_slice();
                self.rank_matching_files(under, &ranking, n, |id| {
                    let suffix = Extension::from_name(name(id)?);
                    Ok(keys.iter().any(|key| suffix.compare_folded(key).is_eq()))
                })
            }
            FileFilter::Kind(kind) => {
                let kind = *kind;
                self.rank_matching_files(under, &ranking, n, |id| {
                    Ok(FileKind::from_extension(&Extension::from_name(name(id)?)) == kind)
                })
            }
        }
    }

    fn rank_matching_files(
        &self,
        under: NodeId,
        ranking: &Ranking<'_>,
        n: usize,
        matches: impl Fn(NodeId) -> Result<bool, QueryError>,
    ) -> Result<Vec<NodeId>, QueryError> {
        let mut result = Vec::new();
        self.visit_preorder(under, |id, node| {
            if node.is_primary_file() && matches(id)? {
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

    /// Searches descendant names in pre-order, preserving the stored order of children.
    ///
    /// The scope itself is excluded. Files, directories, synthetic directories and duplicate hard links can all match. `offset` skips matches; at most `limit` identifiers are retained while the traversal counts every match. An offset past the end returns an empty page with the full count. A zero limit returns only the count.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidNode`] for an unknown scope, or an allocation, overflow or snapshot error during traversal.
    pub fn search(
        &self,
        under: NodeId,
        pattern: &NamePattern,
        offset: usize,
        limit: usize,
    ) -> Result<SearchPage, QueryError> {
        self.check_scope(under)?;
        let skip = u64::try_from(offset).map_err(|_| SnapshotError::overflow("search offset"))?;
        let mut page = SearchPage {
            items: Vec::new(),
            total_matches: 0,
            offset,
        };
        self.visit_preorder(under, |id, _| {
            if id != under && pattern.matches(self.query_name(id)?) {
                if page.total_matches >= skip && page.items.len() < limit {
                    page.items.try_reserve(1)?;
                    page.items.push(id);
                }
                page.total_matches = page
                    .total_matches
                    .checked_add(1)
                    .ok_or_else(|| SnapshotError::overflow("search match count"))?;
            }
            Ok(())
        })?;
        Ok(page)
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
        self.visit_preorder(under, |id, node| {
            if node.is_primary_file() {
                visit(id, node, self.query_name(id)?)?;
            }
            Ok(())
        })
    }

    fn query_name(&self, id: NodeId) -> Result<&str, QueryError> {
        self.name(id)
            .ok_or_else(|| SnapshotError::invalid("missing query name").into())
    }

    fn visit_preorder(
        &self,
        under: NodeId,
        mut visit: impl FnMut(NodeId, &Node) -> Result<(), QueryError>,
    ) -> Result<(), QueryError> {
        let mut pending = Vec::new();
        pending.try_reserve(1)?;
        pending.push(under);
        while let Some(id) = pending.pop() {
            let node = self
                .node(id)
                .ok_or_else(|| SnapshotError::invalid("missing query node"))?;
            visit(id, node.node)?;
            let children = self
                .children(id)
                .ok_or_else(|| SnapshotError::invalid("missing query children"))?;
            pending.try_reserve(children.len())?;
            pending.extend(children.iter().rev().copied());
        }
        Ok(())
    }
}
