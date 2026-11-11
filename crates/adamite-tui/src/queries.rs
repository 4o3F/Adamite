use adamite_core::snapshot::{
    ExtensionStat, FileFilter, KindStat, NamePattern, NodeId, QueryError, SearchPage, Snapshot,
};
use std::borrow::Cow;

pub(crate) const SEARCH_LIMIT: usize = 1000;

#[derive(Debug)]
pub(crate) enum Groups<'a> {
    Extensions(Cow<'a, [ExtensionStat]>),
    Kinds(Vec<KindStat>),
}

/// Cached query results; root indexes remain borrowed until a query needs owned results.
#[derive(Debug)]
pub(crate) struct Queries<'a> {
    pub(crate) scope: NodeId,
    pub(crate) filter: FileFilter,
    pub(crate) top: Cow<'a, [NodeId]>,
    pub(crate) groups: Groups<'a>,
    pub(crate) pattern: Option<NamePattern>,
    pub(crate) search: Option<SearchPage>,
}

impl<'a> Queries<'a> {
    pub(crate) fn new(snapshot: &'a Snapshot) -> Self {
        Self {
            scope: NodeId::ROOT,
            filter: FileFilter::All,
            top: Cow::Borrowed(snapshot.top_files()),
            groups: Groups::Extensions(Cow::Borrowed(snapshot.ext_stats())),
            pattern: None,
            search: None,
        }
    }

    fn rank(
        snapshot: &'a Snapshot,
        scope: NodeId,
        filter: &FileFilter,
    ) -> Result<Cow<'a, [NodeId]>, QueryError> {
        if scope == NodeId::ROOT && matches!(filter, FileFilter::All) {
            Ok(Cow::Borrowed(snapshot.top_files()))
        } else {
            Ok(Cow::Owned(snapshot.rank_files(scope, filter, 100)?))
        }
    }

    fn extensions(snapshot: &'a Snapshot, scope: NodeId) -> Result<Groups<'a>, QueryError> {
        Ok(Groups::Extensions(if scope == NodeId::ROOT {
            Cow::Borrowed(snapshot.ext_stats())
        } else {
            Cow::Owned(snapshot.extension_stats_under(scope)?)
        }))
    }

    pub(crate) fn set_scope(
        &mut self,
        snapshot: &'a Snapshot,
        scope: NodeId,
    ) -> Result<(), QueryError> {
        let top = Self::rank(snapshot, scope, &self.filter)?;
        let groups = match self.groups {
            Groups::Extensions(_) => Self::extensions(snapshot, scope)?,
            Groups::Kinds(_) => Groups::Kinds(snapshot.kind_stats(scope)?),
        };
        let search = self
            .pattern
            .as_ref()
            .map(|pattern| snapshot.search(scope, pattern, 0, SEARCH_LIMIT))
            .transpose()?;
        self.scope = scope;
        self.top = top;
        self.groups = groups;
        self.search = search;
        Ok(())
    }

    pub(crate) fn set_filter(
        &mut self,
        snapshot: &'a Snapshot,
        filter: FileFilter,
    ) -> Result<(), QueryError> {
        if self.filter == filter {
            return Ok(());
        }
        let top = Self::rank(snapshot, self.scope, &filter)?;
        self.filter = filter;
        self.top = top;
        Ok(())
    }

    pub(crate) fn filter_input(&self) -> String {
        match &self.filter {
            FileFilter::All => String::new(),
            FileFilter::Kind(kind) => format!("kind:{}", kind.name()),
            FileFilter::Extensions(keys) if keys.iter().all(String::is_empty) => ",".to_owned(),
            FileFilter::Extensions(keys) => keys.join(","),
            _ => String::new(),
        }
    }

    pub(crate) fn filter_label(&self) -> Option<String> {
        match &self.filter {
            FileFilter::All => None,
            FileFilter::Extensions(keys) => Some(
                keys.iter()
                    .map(|key| {
                        if key.is_empty() {
                            "(none)"
                        } else {
                            key.as_str()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(","),
            ),
            _ => Some(self.filter_input()),
        }
    }

    pub(crate) fn toggle_groups(&mut self, snapshot: &'a Snapshot) -> Result<(), QueryError> {
        self.groups = match self.groups {
            Groups::Extensions(_) => Groups::Kinds(snapshot.kind_stats(self.scope)?),
            Groups::Kinds(_) => Self::extensions(snapshot, self.scope)?,
        };
        Ok(())
    }

    pub(crate) fn group_count(&self) -> usize {
        match &self.groups {
            Groups::Extensions(stats) => stats.len(),
            Groups::Kinds(stats) => stats.len(),
        }
    }

    pub(crate) fn search_items(&self) -> &[NodeId] {
        self.search.as_ref().map_or(&[], SearchPage::items)
    }

    pub(crate) fn search(&mut self, snapshot: &'a Snapshot, value: &str) -> Result<(), QueryError> {
        if self
            .pattern
            .as_ref()
            .is_some_and(|pattern| pattern.as_str() == value)
        {
            return Ok(());
        }
        let pattern = NamePattern::glob(value)?;
        let page = snapshot.search(self.scope, &pattern, 0, SEARCH_LIMIT)?;
        self.pattern = Some(pattern);
        self.search = Some(page);
        Ok(())
    }
}
