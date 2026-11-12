//! Volume-relative path resolution over immutable snapshot children.

use super::{NodeId, QueryError, Snapshot, SnapshotError};
use crate::util::StrExt;

impl Snapshot {
    /// Resolves a path from the snapshot root, preferring exact names over Unicode lowercase matches.
    ///
    /// Both `/` and `\` separate components, and empty components are skipped. Empty paths resolve to [`NodeId::ROOT`]. A component starting with `#` must contain an ASCII decimal identifier of a direct child. The names `.` and `..` are literal names. The final node may be a file.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::PathNotFound`] for an unmatched component, [`QueryError::AmbiguousPath`] with the matching child identifiers for multiple matches, or [`QueryError::NotADirectory`] when descending through a file. Allocation and snapshot errors are propagated.
    pub fn resolve_path(&self, path: &str) -> Result<NodeId, QueryError> {
        let mut current = NodeId::ROOT;
        for component in path.split(['/', '\\']).filter(|part| !part.is_empty()) {
            let node = self
                .node(current)
                .ok_or_else(|| SnapshotError::invalid("missing path node"))?;
            if !node.is_dir() {
                return Err(QueryError::NotADirectory { node: current });
            }
            let children = self
                .children(current)
                .ok_or_else(|| SnapshotError::invalid("missing path children"))?;
            current = self.resolve_component(children, path, component)?;
        }
        Ok(current)
    }

    fn resolve_component(
        &self,
        children: &[NodeId],
        path: &str,
        component: &str,
    ) -> Result<NodeId, QueryError> {
        let matched = if let Some(raw) = component.strip_prefix('#') {
            raw.parse::<u32>()
                .ok()
                .filter(|_| !raw.is_empty() && raw.chars().all(|c| c.is_ascii_digit()))
                .map(NodeId::from_raw)
                .filter(|id| children.contains(id))
        } else if let Some(id) =
            self.unique_path_child(children, path, component, |name| name == component)?
        {
            Some(id)
        } else {
            self.unique_path_child(children, path, component, |name| {
                name.chars()
                    .flat_map(char::to_lowercase)
                    .eq(component.chars().flat_map(char::to_lowercase))
            })?
        };
        match matched {
            Some(id) => Ok(id),
            None => Err(QueryError::PathNotFound {
                path: path.try_copy()?,
                component: component.try_copy()?,
            }),
        }
    }

    fn unique_path_child(
        &self,
        children: &[NodeId],
        path: &str,
        component: &str,
        matches: impl Fn(&str) -> bool,
    ) -> Result<Option<NodeId>, QueryError> {
        let mut candidates = Vec::new();
        for &id in children {
            if self.name(id).is_some_and(&matches) {
                candidates.try_reserve(1)?;
                candidates.push(id);
            }
        }
        match candidates.as_slice() {
            [] => Ok(None),
            [id] => Ok(Some(*id)),
            _ => Err(QueryError::AmbiguousPath {
                path: path.try_copy()?,
                component: component.try_copy()?,
                candidates,
            }),
        }
    }
}
