use crate::{
    input::{Event, InputLine, Key, Prompt},
    queries::{Groups, Queries},
    view::View,
};
use adamite_core::{
    scan::ScanOutput,
    snapshot::{FileFilter, FileKind, NodeId, NodeView, QueryError, Snapshot},
};
use adamite_format::InfoFields;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    Continue,
    Quit,
    Home,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tab {
    #[default]
    Tree,
    TopFiles,
    Extensions,
    Info,
    Search,
}

impl Tab {
    pub(crate) const ALL: [Self; 5] = [
        Self::Tree,
        Self::TopFiles,
        Self::Extensions,
        Self::Info,
        Self::Search,
    ];

    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::Tree => "Tree",
            Self::TopFiles => "Top files",
            Self::Extensions => "Extensions",
            Self::Info => "Info",
            Self::Search => "Search",
        }
    }

    pub(crate) fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|&tab| tab == self)
            .unwrap_or_default()
    }

    fn next(self) -> Self {
        Self::ALL
            .get(self.index().saturating_add(1))
            .copied()
            .unwrap_or_default()
    }
}

/// Selection and viewport offset belong to a view, independently of its contents.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Cursor {
    pub(crate) selected: usize,
    pub(crate) scroll: usize,
}

impl Cursor {
    pub(crate) fn move_to(&mut self, selected: usize, count: usize, page_rows: usize) {
        self.selected = selected.min(count.saturating_sub(1));
        let page_rows = page_rows.max(1);
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected.saturating_sub(self.scroll) >= page_rows {
            self.scroll = self.selected.saturating_add(1).saturating_sub(page_rows);
        }
        self.scroll = self.scroll.min(count.saturating_sub(page_rows));
    }

    pub(crate) fn handle(&mut self, key: Key, count: usize, page_rows: usize) {
        let next = match key {
            Key::Up => self.selected.saturating_sub(1),
            Key::Down => self.selected.saturating_add(1),
            Key::PageUp => self.selected.saturating_sub(page_rows),
            Key::PageDown => self.selected.saturating_add(page_rows),
            Key::Home => 0,
            Key::End => count.saturating_sub(1),
            _ => return,
        };
        self.move_to(next, count, page_rows);
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct DirectoryView {
    pub(crate) directory: NodeId,
    pub(crate) cursor: Cursor,
}

impl DirectoryView {
    fn new(directory: NodeId) -> Self {
        Self {
            directory,
            cursor: Cursor::default(),
        }
    }

    fn select(&mut self, snapshot: &Snapshot, child: NodeId, page_rows: usize) {
        let children = snapshot.children(self.directory).unwrap_or_default();
        let selected = children
            .iter()
            .position(|&id| id == child)
            .unwrap_or_default();
        self.cursor.move_to(selected, children.len(), page_rows);
    }

    fn clamp(&mut self, snapshot: &Snapshot, page_rows: usize) {
        let count = snapshot.children(self.directory).map_or(0, <[NodeId]>::len);
        self.cursor.move_to(self.cursor.selected, count, page_rows);
    }
}

/// Pure navigation state; terminal I/O and drawing belong to Browser and View.
#[derive(Debug)]
pub(crate) struct App<'a> {
    pub(crate) output: &'a ScanOutput,
    pub(crate) tree: DirectoryView,
    pub(crate) tab: Tab,
    pub(crate) top_files: Cursor,
    pub(crate) extensions: Cursor,
    pub(crate) info: Cursor,
    pub(crate) search: Cursor,
    pub(crate) queries: Queries<'a>,
    pub(crate) input: Option<InputLine>,
    pub(crate) status: Option<String>,
    pub(crate) raw_bytes: bool,
    pub(crate) help: bool,
    pub(crate) return_home: bool,
    parents: Vec<DirectoryView>,
    page_rows: usize,
}

impl<'a> App<'a> {
    pub(crate) fn new(output: &'a ScanOutput, start: NodeId, page_rows: usize) -> Self {
        let start = if output.snapshot.node(start).is_some_and(NodeView::is_dir) {
            start
        } else {
            NodeId::ROOT
        };
        Self {
            output,
            tree: DirectoryView::new(start),
            tab: Tab::default(),
            top_files: Cursor::default(),
            extensions: Cursor::default(),
            info: Cursor::default(),
            search: Cursor::default(),
            queries: Queries::new(&output.snapshot),
            input: None,
            status: None,
            raw_bytes: false,
            help: false,
            return_home: false,
            parents: Vec::new(),
            page_rows: page_rows.max(1),
        }
    }

    pub(crate) fn snapshot(&self) -> &'a Snapshot {
        &self.output.snapshot
    }

    pub(crate) fn children(&self) -> &'a [NodeId] {
        self.snapshot()
            .children(self.tree.directory)
            .unwrap_or_default()
    }

    pub(crate) fn info_fields(&self) -> Vec<(&'static str, String)> {
        InfoFields {
            output: self.output,
            raw_bytes: self.raw_bytes,
        }
        .rows()
    }

    pub(crate) fn handle_terminal(&mut self, event: ratatui::crossterm::event::Event) -> Action {
        if let Some(input) = &mut self.input
            && matches!(event, ratatui::crossterm::event::Event::Key(_))
        {
            return input
                .handle(&event)
                .map_or(Action::Continue, |key| self.handle(Event::Key(key)));
        }
        Event::from_terminal(event).map_or(Action::Continue, |event| self.handle(event))
    }

    fn visible_rows(&self) -> usize {
        self.page_rows
            .saturating_sub(usize::from(self.input.is_some()))
            .max(1)
    }

    pub(crate) fn handle(&mut self, event: Event) -> Action {
        let key = match event {
            Event::Resize { height, .. } => {
                self.resize(View::row_count(height));
                return Action::Continue;
            }
            Event::Key(key) => key,
        };
        if self.input.is_some() {
            match key {
                Key::Escape => {
                    self.input = None;
                    self.resize(self.page_rows);
                }
                Key::Enter => self.confirm_input(),
                Key::NextTab => {
                    self.input = None;
                    self.show_tab(self.tab.next());
                    self.resize(self.page_rows);
                }
                _ => {}
            }
            return Action::Continue;
        }
        match key {
            Key::Quit => return Action::Quit,
            Key::ReturnHome if self.return_home && !self.help => return Action::Home,
            Key::Escape if !self.help => return Action::Quit,
            Key::Help | Key::Escape => self.help = !self.help,
            _ if self.help => {}
            Key::NextTab => self.show_tab(self.tab.next()),
            Key::SelectTab(tab) => self.show_tab(tab),
            Key::Bytes => self.raw_bytes = !self.raw_bytes,
            Key::Scope => self.toggle_scope(),
            Key::Filter if self.tab == Tab::TopFiles => self.open_input(Prompt::Filter),
            Key::Search => {
                self.tab = Tab::Search;
                self.open_input(Prompt::Search);
            }
            Key::Group if self.tab == Tab::Extensions => {
                let result = self.queries.toggle_groups(&self.output.snapshot);
                if self.query_result(result) {
                    self.extensions = Cursor::default();
                }
            }
            Key::Enter if self.tab == Tab::Extensions => self.select_group(),
            Key::Enter | Key::Open if self.tab == Tab::Tree => self.enter(),
            Key::Enter if self.tab == Tab::Search && self.queries.search_items().is_empty() => {
                self.open_input(Prompt::Search);
            }
            Key::Enter if self.tab == Tab::TopFiles || self.tab == Tab::Search => {
                let id = if self.tab == Tab::TopFiles {
                    self.queries.top.get(self.top_files.selected)
                } else {
                    self.queries.search_items().get(self.search.selected)
                }
                .copied();
                if let Some(id) = id {
                    self.jump_to(id);
                }
            }
            Key::Parent if self.tab == Tab::Tree => self.parent(),
            _ => {
                let page_rows = self.visible_rows();
                let (cursor, count) = match self.tab {
                    Tab::Tree => {
                        let count = self.children().len();
                        (&mut self.tree.cursor, count)
                    }
                    Tab::TopFiles => (&mut self.top_files, self.queries.top.len()),
                    Tab::Extensions => (&mut self.extensions, self.queries.group_count()),
                    Tab::Info => {
                        let count = self.info_fields().len();
                        (&mut self.info, count)
                    }
                    Tab::Search => (&mut self.search, self.queries.search_items().len()),
                };
                cursor.handle(key, count, page_rows);
            }
        }
        Action::Continue
    }

    fn resize(&mut self, page_rows: usize) {
        self.page_rows = page_rows.max(1);
        let rows = self.visible_rows();
        self.tree.clamp(&self.output.snapshot, rows);
        for parent in &mut self.parents {
            parent.clamp(&self.output.snapshot, rows);
        }
        self.top_files
            .move_to(self.top_files.selected, self.queries.top.len(), rows);
        self.extensions
            .move_to(self.extensions.selected, self.queries.group_count(), rows);
        self.info
            .move_to(self.info.selected, self.info_fields().len(), rows);
        self.search.move_to(
            self.search.selected,
            self.queries.search_items().len(),
            rows,
        );
    }

    fn enter(&mut self) {
        if let Some(&id) = self.children().get(self.tree.cursor.selected)
            && self.snapshot().node(id).is_some_and(NodeView::is_dir)
        {
            self.parents.push(self.tree);
            self.tree = DirectoryView::new(id);
        }
    }

    fn parent(&mut self) {
        if let Some(parent) = self.parents.pop() {
            self.tree = parent;
        } else if let Some(node) = self.snapshot().node(self.tree.directory)
            && node.parent() != self.tree.directory
        {
            let child = self.tree.directory;
            self.tree = DirectoryView::new(node.parent());
            self.tree
                .select(&self.output.snapshot, child, self.page_rows);
        }
    }

    fn query_result(&mut self, result: Result<(), QueryError>) -> bool {
        match result {
            Ok(()) => {
                self.status = None;
                true
            }
            Err(error) => {
                self.status = Some(error.to_string());
                false
            }
        }
    }

    fn toggle_scope(&mut self) {
        let scope = if self.queries.scope == NodeId::ROOT {
            self.tree.directory
        } else {
            NodeId::ROOT
        };
        if scope == self.queries.scope {
            return;
        }
        let result = self.queries.set_scope(&self.output.snapshot, scope);
        if self.query_result(result) {
            self.top_files = Cursor::default();
            self.extensions = Cursor::default();
            self.search = Cursor::default();
        }
    }

    fn show_tab(&mut self, tab: Tab) {
        self.tab = tab;
        if tab == Tab::Search && self.queries.pattern.is_none() {
            self.open_input(Prompt::Search);
        }
    }

    fn open_input(&mut self, prompt: Prompt) {
        let value = match prompt {
            Prompt::Filter => self.queries.filter_input(),
            Prompt::Search => self
                .queries
                .pattern
                .as_ref()
                .map_or_else(String::new, |pattern| pattern.as_str().to_owned()),
        };
        self.input = Some(InputLine::new(prompt, value));
        self.status = None;
        self.resize(self.page_rows);
    }

    fn confirm_input(&mut self) {
        let Some(input) = self.input.take() else {
            return;
        };
        let value = input.editor.value();
        let result = match input.prompt {
            Prompt::Filter => {
                let value = value.trim();
                let filter = if value.is_empty() {
                    Ok(FileFilter::All)
                } else if let Some(kind) = value.strip_prefix("kind:") {
                    kind.parse::<FileKind>().map(FileFilter::Kind)
                } else {
                    FileFilter::extensions(value.split(',').map(str::trim))
                };
                filter.and_then(|filter| self.queries.set_filter(&self.output.snapshot, filter))
            }
            Prompt::Search => self.queries.search(&self.output.snapshot, value),
        };
        if self.query_result(result) {
            match input.prompt {
                Prompt::Filter => self.top_files = Cursor::default(),
                Prompt::Search => self.search = Cursor::default(),
            }
        }
        self.resize(self.page_rows);
    }

    fn select_group(&mut self) {
        let filter = match &self.queries.groups {
            Groups::Extensions(stats) => stats
                .get(self.extensions.selected)
                .map(|stat| FileFilter::extensions([stat.extension()])),
            Groups::Kinds(stats) => stats
                .get(self.extensions.selected)
                .map(|stat| Ok(FileFilter::Kind(stat.kind()))),
        };
        if let Some(filter) = filter {
            let result =
                filter.and_then(|filter| self.queries.set_filter(&self.output.snapshot, filter));
            if self.query_result(result) {
                self.top_files = Cursor::default();
                self.tab = Tab::TopFiles;
            }
        }
    }

    fn jump_to(&mut self, node: NodeId) {
        let mut child = node;
        let mut lineage = Vec::new();
        // Snapshot validation guarantees a rooted tree; keep traversal bounded as well.
        for _ in 0..self.snapshot().len() {
            if child == NodeId::ROOT {
                break;
            }
            let Some(node) = self.snapshot().node(child) else {
                return;
            };
            let mut parent = DirectoryView::new(node.parent());
            parent.select(self.snapshot(), child, self.page_rows);
            lineage.push(parent);
            child = node.parent();
        }
        if child != NodeId::ROOT {
            return;
        }
        lineage.reverse();
        if let Some(directory) = lineage.pop() {
            self.tree = directory;
            self.parents = lineage;
            self.tab = Tab::Tree;
        }
    }
}
