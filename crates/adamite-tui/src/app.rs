use crate::input::{Event, Key};
use adamite_core::{
    scan::ScanOutput,
    snapshot::{NodeId, NodeView, Snapshot},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    Continue,
    Quit,
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
}

/// Pure navigation state; terminal I/O and drawing belong to Browser and View.
#[derive(Debug)]
pub(crate) struct App<'a> {
    pub(crate) output: &'a ScanOutput,
    pub(crate) tree: DirectoryView,
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

    pub(crate) fn handle(&mut self, event: Event) -> Action {
        let Event::Key(key) = event;
        let selected = self.tree.cursor.selected;
        let count = self.children().len();
        let next = match key {
            Key::Quit => return Action::Quit,
            Key::Enter => {
                self.enter();
                return Action::Continue;
            }
            Key::Parent => {
                self.parent();
                return Action::Continue;
            }
            Key::Up => selected.saturating_sub(1),
            Key::Down => selected.saturating_add(1),
            Key::PageUp => selected.saturating_sub(self.page_rows),
            Key::PageDown => selected.saturating_add(self.page_rows),
            Key::Home => 0,
            Key::End => count.saturating_sub(1),
        };
        self.tree.cursor.move_to(next, count, self.page_rows);
        Action::Continue
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
            let selected = self
                .children()
                .iter()
                .position(|&id| id == child)
                .unwrap_or_default();
            self.tree
                .cursor
                .move_to(selected, self.children().len(), self.page_rows);
        }
    }
}
