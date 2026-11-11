use crate::{
    input::{Event, Key},
    view::View,
};
use adamite_core::{
    scan::ScanOutput,
    snapshot::{NodeId, NodeView, Snapshot},
};
use adamite_format::InfoFields;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    Continue,
    Quit,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tab {
    #[default]
    Tree,
    TopFiles,
    Extensions,
    Info,
}

impl Tab {
    pub(crate) const ALL: [Self; 4] = [Self::Tree, Self::TopFiles, Self::Extensions, Self::Info];

    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::Tree => "Tree",
            Self::TopFiles => "Top files",
            Self::Extensions => "Extensions",
            Self::Info => "Info",
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
    fn move_to(&mut self, selected: usize, count: usize, page_rows: usize) {
        self.selected = selected.min(count.saturating_sub(1));
        let page_rows = page_rows.max(1);
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected.saturating_sub(self.scroll) >= page_rows {
            self.scroll = self.selected.saturating_add(1).saturating_sub(page_rows);
        }
        self.scroll = self.scroll.min(count.saturating_sub(page_rows));
    }

    fn handle(&mut self, key: Key, count: usize, page_rows: usize) {
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
    pub(crate) raw_bytes: bool,
    pub(crate) help: bool,
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
            raw_bytes: false,
            help: false,
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

    pub(crate) fn handle(&mut self, event: Event) -> Action {
        let key = match event {
            Event::Resize { height, .. } => {
                self.resize(View::row_count(height));
                return Action::Continue;
            }
            Event::Key(key) => key,
        };
        match key {
            Key::Quit => return Action::Quit,
            Key::Escape if !self.help => return Action::Quit,
            Key::Help | Key::Escape => self.help = !self.help,
            _ if self.help => {}
            Key::NextTab => self.tab = self.tab.next(),
            Key::SelectTab(tab) => self.tab = tab,
            Key::Bytes => self.raw_bytes = !self.raw_bytes,
            Key::Enter | Key::Open if self.tab == Tab::Tree => self.enter(),
            Key::Enter if self.tab == Tab::TopFiles => self.jump_to_file(),
            Key::Parent if self.tab == Tab::Tree => self.parent(),
            _ => {
                let (cursor, count) = match self.tab {
                    Tab::Tree => {
                        let count = self.children().len();
                        (&mut self.tree.cursor, count)
                    }
                    Tab::TopFiles => (&mut self.top_files, self.output.snapshot.top_files().len()),
                    Tab::Extensions => {
                        (&mut self.extensions, self.output.snapshot.ext_stats().len())
                    }
                    Tab::Info => {
                        let count = self.info_fields().len();
                        (&mut self.info, count)
                    }
                };
                cursor.handle(key, count, self.page_rows);
            }
        }
        Action::Continue
    }

    fn resize(&mut self, page_rows: usize) {
        self.page_rows = page_rows.max(1);
        self.tree.clamp(&self.output.snapshot, self.page_rows);
        for parent in &mut self.parents {
            parent.clamp(&self.output.snapshot, self.page_rows);
        }
        self.top_files.move_to(
            self.top_files.selected,
            self.snapshot().top_files().len(),
            self.page_rows,
        );
        self.extensions.move_to(
            self.extensions.selected,
            self.snapshot().ext_stats().len(),
            self.page_rows,
        );
        self.info
            .move_to(self.info.selected, self.info_fields().len(), self.page_rows);
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

    fn jump_to_file(&mut self) {
        let Some(&file) = self.snapshot().top_files().get(self.top_files.selected) else {
            return;
        };
        let mut child = file;
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
