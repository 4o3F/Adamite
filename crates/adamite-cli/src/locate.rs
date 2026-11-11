use crate::error::CliError;
use adamite_core::snapshot::{NodeId, NodeView, Snapshot};

/// Resolves snapshot paths; #id selects only a direct child of the current directory.
pub(crate) struct Locator<'a> {
    snapshot: &'a Snapshot,
}

impl<'a> Locator<'a> {
    pub(crate) fn new(snapshot: &'a Snapshot) -> Self {
        Self { snapshot }
    }

    pub(crate) fn resolve(&self, path: &str) -> Result<NodeId, CliError> {
        let mut current = NodeId::ROOT;
        for component in path.split(['/', '\\']).filter(|part| !part.is_empty()) {
            let children = self.directory(current)?;
            current = self.component(children, path, component)?;
        }
        Ok(current)
    }

    pub(crate) fn directory(&self, id: NodeId) -> Result<&'a [NodeId], CliError> {
        if self.snapshot.node(id).is_some_and(NodeView::is_dir)
            && let Some(children) = self.snapshot.children(id)
        {
            return Ok(children);
        }
        let path = self
            .snapshot
            .path(id)
            .map_err(|source| CliError::Snapshot { source })?
            .ok_or_else(|| CliError::PathNotFound {
                path: format!("#{}", id.raw()),
                component: format!("#{}", id.raw()),
            })?;
        Err(CliError::NotADirectory { path })
    }

    fn component(
        &self,
        children: &[NodeId],
        path: &str,
        component: &str,
    ) -> Result<NodeId, CliError> {
        if let Some(raw) = component.strip_prefix('#') {
            let id = raw
                .parse::<u32>()
                .ok()
                .filter(|_| !raw.is_empty() && raw.chars().all(|c| c.is_ascii_digit()))
                .map(NodeId::from_raw)
                .filter(|id| children.contains(id));
            return id.ok_or_else(|| self.not_found(path, component));
        }
        if let Some(id) = self.unique(children, path, component, |name| name == component)? {
            return Ok(id);
        }
        self.unique(children, path, component, |name| {
            name.chars()
                .flat_map(char::to_lowercase)
                .eq(component.chars().flat_map(char::to_lowercase))
        })?
        .ok_or_else(|| self.not_found(path, component))
    }

    fn unique(
        &self,
        children: &[NodeId],
        path: &str,
        component: &str,
        matches: impl Fn(&str) -> bool,
    ) -> Result<Option<NodeId>, CliError> {
        let candidates: Vec<_> = children
            .iter()
            .copied()
            .filter(|id| self.snapshot.name(*id).is_some_and(&matches))
            .collect();
        match candidates.as_slice() {
            [] => Ok(None),
            [id] => Ok(Some(*id)),
            _ => Err(CliError::AmbiguousPath {
                path: path.to_owned(),
                component: component.to_owned(),
                candidates,
            }),
        }
    }

    fn not_found(&self, path: &str, component: &str) -> CliError {
        CliError::PathNotFound {
            path: path.to_owned(),
            component: component.to_owned(),
        }
    }
}
