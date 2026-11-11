use crate::{
    app::{App, Cursor, Tab},
    input::InputLine,
    queries::Groups,
};
use adamite_core::snapshot::{NodeFlags, NodeId, NodeView};
use adamite_format::{ByteSize, Escaped, ExtensionLabel, ScanTime};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    widgets::{Block, Cell, Clear, Paragraph, Row, Table, Tabs},
};
use std::fmt;
use unicode_width::UnicodeWidthStr;

/// Renders borrowed application state without changing its cursors.
pub(crate) struct View<'a> {
    app: &'a App<'a>,
}

impl<'a> View<'a> {
    // One row each for path, tabs, column headings and the key hints.
    const CHROME_ROWS: u16 = 4;

    pub(crate) fn new(app: &'a App<'a>) -> Self {
        Self { app }
    }

    pub(crate) fn row_count(height: u16) -> usize {
        usize::from(height.saturating_sub(Self::CHROME_ROWS))
    }

    pub(crate) fn render(&self, frame: &mut Frame<'_>) {
        let _entered = tracing::info_span!(
            "tui_frame_render",
            width = frame.area().width,
            height = frame.area().height
        )
        .entered();
        if too_small(frame) {
            return;
        }
        let [header, tabs, content, prompt, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(u16::from(self.app.input.is_some())),
            Constraint::Length(1),
        ])
        .areas(frame.area());
        let mut heading = if self.app.tab == Tab::Tree {
            self.path(self.app.tree.directory)
        } else {
            format!(
                "{} — scanned {}",
                self.app.tab.title(),
                ScanTime(self.app.snapshot().volume().context().scanned_at_filetime)
            )
        };
        if self.app.tab == Tab::TopFiles
            && let Some(filter) = self.app.queries.filter_label()
        {
            heading.push_str(&format!(" · filter: {}", Escaped(&filter)));
        }
        if self.app.tab == Tab::Search
            && let Some(pattern) = &self.app.queries.pattern
        {
            heading.push_str(&format!(" · {}", Escaped(pattern.as_str())));
        }
        frame.render_widget(
            Paragraph::new(DisplayText::truncate(
                &heading,
                usize::from(header.width),
                Truncation::Middle,
            )),
            header,
        );
        let scope = if self.app.queries.scope != NodeId::ROOT {
            format!("under {}", self.path(self.app.queries.scope))
        } else {
            String::new()
        };
        let scope_width = u16::try_from(scope.width())
            .unwrap_or(u16::MAX)
            .min(tabs.width.saturating_div(2));
        let [tab_area, scope_area] =
            Layout::horizontal([Constraint::Fill(1), Constraint::Length(scope_width)]).areas(tabs);
        frame.render_widget(
            Tabs::new(Tab::ALL.map(Tab::title))
                .select(self.app.tab.index())
                .highlight_style(Style::new().fg(Color::Cyan)),
            tab_area,
        );
        frame.render_widget(
            Paragraph::new(DisplayText::truncate(
                &scope,
                usize::from(scope_area.width),
                Truncation::Middle,
            )),
            scope_area,
        );
        match self.app.tab {
            Tab::Tree => self.tree(frame, content),
            Tab::TopFiles => self.files(frame, content, self.app.top_files, &self.app.queries.top),
            Tab::Extensions => self.extensions(frame, content),
            Tab::Info => self.info(frame, content),
            Tab::Search => self.files(
                frame,
                content,
                self.app.search,
                self.app.queries.search_items(),
            ),
        }
        if let Some(input) = &self.app.input {
            render_input(frame, prompt, input);
        }
        let status = if let Some(status) = &self.app.status {
            Escaped(status).to_string()
        } else if self.app.input.is_some() {
            "Enter: apply  Esc: cancel  ←/→ Home/End: edit".to_owned()
        } else if self.app.tab == Tab::Search {
            match &self.app.queries.search {
                Some(page)
                    if page.total_matches()
                        > u64::try_from(page.items().len()).unwrap_or(u64::MAX) =>
                {
                    let more = page
                        .total_matches()
                        .saturating_sub(u64::try_from(page.items().len()).unwrap_or(u64::MAX));
                    format!("{more} more, refine the pattern")
                }
                Some(page) if page.items().is_empty() => "no matches".to_owned(),
                _ => "Tab/1–5: tabs  /: search  ?: help  q: quit".to_owned(),
            }
        } else {
            "Tab/1–5: tabs  u: scope  b: bytes  ?: help  q: quit".to_owned()
        };
        let status =
            if self.app.return_home && self.app.input.is_none() && self.app.status.is_none() {
                format!("{status}  o: home")
            } else {
                status
            };
        frame.render_widget(Paragraph::new(status), footer);
        if self.app.help {
            self.help(frame);
        }
    }

    fn path(&self, id: NodeId) -> String {
        let path = self
            .app
            .snapshot()
            .path(id)
            .ok()
            .flatten()
            .unwrap_or_else(|| format!("#{}", id.raw()));
        Escaped(&path).to_string()
    }

    fn size(&self, value: u64) -> String {
        ByteSize {
            value: u128::from(value),
            raw_bytes: self.app.raw_bytes,
        }
        .to_string()
    }

    fn size_width(&self) -> u16 {
        if self.app.raw_bytes { 20 } else { 11 }
    }

    fn files(&self, frame: &mut Frame<'_>, area: Rect, cursor: Cursor, items: &[NodeId]) {
        let widths = [
            Constraint::Fill(1),
            Constraint::Length(self.size_width()),
            Constraint::Length(self.size_width()),
        ];
        let table = Rows {
            cursor,
            widths: &widths,
            headings: &["Path", "Allocated", "Logical"],
        };
        let path_width = table.name_width(area);
        let rows = table.visible(area, items).filter_map(|(index, &id)| {
            let node = self.app.snapshot().node(id)?;
            let mut path = self.path(id);
            if node.is_dir() {
                path.push('/');
            }
            Some(
                Row::new([
                    DisplayText::truncate(&path, usize::from(path_width), Truncation::Middle),
                    self.size(if node.is_dir() {
                        node.total_alloc()
                    } else {
                        node.alloc()
                    }),
                    self.size(if node.is_dir() {
                        node.total_size()
                    } else {
                        node.size()
                    }),
                ])
                .style(row_style(index == table.cursor.selected)),
            )
        });
        table.render(frame, area, rows);
    }

    fn extensions(&self, frame: &mut Frame<'_>, area: Rect) {
        let widths = [
            Constraint::Fill(1),
            Constraint::Length(10),
            Constraint::Length(self.size_width()),
            Constraint::Length(self.size_width()),
        ];
        let kinds = matches!(self.app.queries.groups, Groups::Kinds(_));
        let table = Rows {
            cursor: self.app.extensions,
            widths: &widths,
            headings: &[
                if kinds { "Kind" } else { "Extension" },
                "Count",
                "Logical",
                "Allocated",
            ],
        };
        let name_width = table.name_width(area);
        let row = |index, label: String, count: u64, logical: u64, allocated: u64| {
            Row::new([
                DisplayText::truncate(&label, usize::from(name_width), Truncation::End),
                count.to_string(),
                self.size(logical),
                self.size(allocated),
            ])
            .style(row_style(index == table.cursor.selected))
        };
        match &self.app.queries.groups {
            Groups::Extensions(stats) => table.render(
                frame,
                area,
                table.visible(area, stats.iter()).map(|(index, stat)| {
                    row(
                        index,
                        ExtensionLabel(stat.extension()).to_string(),
                        stat.count(),
                        stat.total_size(),
                        stat.total_alloc(),
                    )
                }),
            ),
            Groups::Kinds(stats) => table.render(
                frame,
                area,
                table.visible(area, stats.iter()).map(|(index, stat)| {
                    row(
                        index,
                        stat.kind().name().to_owned(),
                        stat.count(),
                        stat.total_size(),
                        stat.total_alloc(),
                    )
                }),
            ),
        }
    }

    fn info(&self, frame: &mut Frame<'_>, area: Rect) {
        let table = Rows {
            cursor: self.app.info,
            widths: &[Constraint::Length(28), Constraint::Fill(1)],
            headings: &["Field", "Value"],
        };
        let rows = table
            .visible(area, self.app.info_fields())
            .map(|(index, (label, value))| {
                Row::new([label.to_owned(), value]).style(row_style(index == table.cursor.selected))
            });
        table.render(frame, area, rows);
    }

    fn help(&self, frame: &mut Frame<'_>) {
        let mut text = concat!(
            "↑/↓ k/j; PgUp/PgDn; Home/End\n",
            "Tab/1–5: tabs; Enter/→/l: open\n",
            "←/h/Backspace: parent; b: raw bytes\n",
            "Enter: jump; u: scope; f: filter\n",
            "Ext g: kind; /: search; q/Ctrl-C: quit\n",
            "?/Esc: close help; Esc: quit",
        )
        .to_owned();
        if self.app.return_home {
            text.push_str("\no: return to Home");
        }
        render_help(frame, &text);
    }

    fn tree(&self, frame: &mut Frame<'_>, area: Rect) {
        let widths = [
            Constraint::Fill(1),
            Constraint::Length(self.size_width()),
            Constraint::Length(20),
            Constraint::Length(self.size_width()),
            Constraint::Length(10),
        ];
        let table = Rows {
            cursor: self.app.tree.cursor,
            widths: &widths,
            headings: &["Name", "Allocated", "Share of parent", "Logical", "Files"],
        };
        let name_width = table.name_width(area);
        let parent_alloc = self
            .app
            .snapshot()
            .node(self.app.tree.directory)
            .map_or(0, |node| node.total_alloc());
        let rows = table
            .visible(area, self.app.children())
            .filter_map(|(index, &id)| {
                let node = self.app.snapshot().node(id)?;
                let name = self.app.snapshot().name(id)?;
                Some(
                    TreeRow {
                        node,
                        name,
                        raw_bytes: self.app.raw_bytes,
                    }
                    .render(
                        name_width,
                        parent_alloc,
                        index == self.app.tree.cursor.selected,
                    ),
                )
            });
        table.render(frame, area, rows);
        if self.app.children().is_empty() {
            let [_, body] =
                Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
            frame.render_widget(Paragraph::new("(empty)"), body);
        }
    }
}

pub(crate) fn too_small(frame: &mut Frame<'_>) -> bool {
    if frame.area().width >= 40 && frame.area().height >= 8 {
        return false;
    }
    frame.render_widget(
        Paragraph::new("terminal too small (minimum 40×8)"),
        frame.area(),
    );
    true
}

pub(crate) fn render_input<P: AsRef<str>>(
    frame: &mut Frame<'_>,
    prompt: Rect,
    input: &InputLine<P>,
) {
    let label = input.prompt.as_ref();
    let label_width = u16::try_from(label.width())
        .unwrap_or(u16::MAX)
        .min(prompt.width);
    let [label_area, editor_area] =
        Layout::horizontal([Constraint::Length(label_width), Constraint::Fill(1)]).areas(prompt);
    frame.render_widget(Paragraph::new(label), label_area);
    let width = usize::from(editor_area.width.saturating_sub(1));
    let scroll = input.editor.visual_scroll(width);
    frame.render_widget(
        Paragraph::new(input.editor.value()).scroll((0, u16::try_from(scroll).unwrap_or(u16::MAX))),
        editor_area,
    );
    let cursor = input
        .editor
        .visual_cursor()
        .saturating_sub(scroll)
        .min(width);
    frame.set_cursor_position((
        editor_area
            .x
            .saturating_add(u16::try_from(cursor).unwrap_or_default()),
        editor_area.y,
    ));
}

pub(crate) fn render_help(frame: &mut Frame<'_>, text: &str) {
    let height = u16::try_from(text.lines().count())
        .unwrap_or(u16::MAX)
        .saturating_add(2);
    let area = frame
        .area()
        .centered(Constraint::Length(40), Constraint::Length(height));
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(text).block(Block::bordered().title("Help")),
        area,
    );
}

pub(crate) fn row_style(selected: bool) -> Style {
    if selected {
        Style::new()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    }
}

pub(crate) struct Rows<'a> {
    pub(crate) cursor: Cursor,
    pub(crate) widths: &'a [Constraint],
    pub(crate) headings: &'a [&'a str],
}

impl Rows<'_> {
    pub(crate) fn visible<T>(
        &self,
        area: Rect,
        items: impl IntoIterator<Item = T>,
    ) -> impl Iterator<Item = (usize, T)> {
        items
            .into_iter()
            .enumerate()
            .skip(self.cursor.scroll)
            .take(usize::from(area.height.saturating_sub(1)))
    }

    pub(crate) fn name_width(&self, area: Rect) -> u16 {
        Layout::horizontal(self.widths)
            .spacing(1)
            .split(area)
            .first()
            .map_or(0, |column| column.width)
    }

    pub(crate) fn render<'a>(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        rows: impl IntoIterator<Item = Row<'a>>,
    ) {
        frame.render_widget(
            Table::new(rows, self.widths).column_spacing(1).header(
                Row::new(self.headings.iter().copied())
                    .style(Style::new().add_modifier(Modifier::BOLD)),
            ),
            area,
        );
    }
}

struct TreeRow<'a> {
    node: NodeView<'a>,
    name: &'a str,
    raw_bytes: bool,
}

impl TreeRow<'_> {
    fn render(&self, name_width: u16, parent_alloc: u64, selected: bool) -> Row<'static> {
        let flags = self.node.flags();
        let mut markers = String::new();
        if self.node.is_dir() {
            markers.push('/');
        }
        if flags.contains(NodeFlags::HARDLINK_DUP) {
            markers.push('=');
        }
        if flags.contains(NodeFlags::REPARSE) {
            markers.push('↪');
        }
        let width = usize::from(name_width);
        let name = DisplayText::truncate(
            &Escaped(self.name).to_string(),
            width.saturating_sub(markers.width()),
            Truncation::End,
        );
        let name = format!("{name}{markers}");
        let share = Share::new(self.node.total_alloc(), parent_alloc);
        let share = if flags.contains(NodeFlags::HARDLINK_DUP) {
            "—".to_owned()
        } else {
            share.to_string()
        };
        let mut style = row_style(selected);
        if flags.contains(NodeFlags::SYNTHETIC) {
            style = style.add_modifier(Modifier::DIM);
        }
        Row::new([
            Cell::from(name),
            Cell::from(
                ByteSize {
                    value: u128::from(self.node.total_alloc()),
                    raw_bytes: self.raw_bytes,
                }
                .to_string(),
            ),
            Cell::from(share),
            Cell::from(
                ByteSize {
                    value: u128::from(self.node.total_size()),
                    raw_bytes: self.raw_bytes,
                }
                .to_string(),
            ),
            Cell::from(self.node.file_count().to_string()),
        ])
        .style(style)
    }
}

struct Share {
    allocated: u64,
    parent_allocated: u64,
}

impl Share {
    const BAR_WIDTH: usize = 10;

    fn new(allocated: u64, parent_allocated: u64) -> Self {
        Self {
            allocated,
            parent_allocated,
        }
    }

    fn scaled(&self, scale: u128) -> u128 {
        u128::from(self.allocated)
            .checked_mul(scale)
            .and_then(|value| value.checked_div(u128::from(self.parent_allocated)))
            .unwrap_or_default()
            .min(scale)
    }
}

impl fmt::Display for Share {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let filled = usize::try_from(self.scaled(Self::BAR_WIDTH as u128))
            .unwrap_or_default()
            .min(Self::BAR_WIDTH);
        let label = if self.parent_allocated == 0 {
            "—".to_owned()
        } else {
            format!("{}‰", self.scaled(1000))
        };
        write!(
            f,
            "{label:>5} [{}{}]",
            "█".repeat(filled),
            " ".repeat(Self::BAR_WIDTH.saturating_sub(filled))
        )
    }
}

pub(crate) enum Truncation {
    End,
    Middle,
}

pub(crate) struct DisplayText;

impl DisplayText {
    fn prefix(text: &str, width: usize) -> String {
        let mut result = String::new();
        for character in text.chars() {
            result.push(character);
            if result.width() > width {
                result.pop();
                break;
            }
        }
        result
    }

    fn suffix(text: &str, width: usize) -> &str {
        let mut result = "";
        for (start, _) in text.char_indices().rev() {
            let Some(suffix) = text.get(start..) else {
                break;
            };
            if suffix.width() > width {
                break;
            }
            result = suffix;
        }
        result
    }

    pub(crate) fn truncate(text: &str, width: usize, placement: Truncation) -> String {
        if text.width() <= width {
            return text.to_owned();
        }
        if width == 0 {
            return String::new();
        }
        let available = width.saturating_sub(1);
        match placement {
            Truncation::End => format!("{}…", Self::prefix(text, available)),
            Truncation::Middle => {
                let right = available / 2;
                let left = available.saturating_sub(right);
                format!("{}…{}", Self::prefix(text, left), Self::suffix(text, right))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{DisplayText, Share, Truncation};
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn shares_use_wide_arithmetic_and_clamp_bars() {
        for (allocated, parent, expected) in [
            (u64::MAX, u64::MAX, "1000‰ [██████████]"),
            (u64::MAX, 1, "1000‰ [██████████]"),
            (1, 3, " 333‰ [███       ]"),
            (0, 3, "   0‰ [          ]"),
            (0, 0, "    — [          ]"),
        ] {
            assert_eq!(Share::new(allocated, parent).to_string(), expected);
        }
    }

    #[test]
    fn truncation_respects_cell_width_for_wide_and_combining_text() {
        for (text, width, expected) in [
            ("漢字abc", 4, "漢…"),
            ("cafe\u{301}-long", 5, "cafe\u{301}…"),
            ("hello", 0, ""),
            ("hello", 1, "…"),
            ("漢字", 4, "漢字"),
        ] {
            let truncated = DisplayText::truncate(text, width, Truncation::End);
            assert_eq!(truncated, expected);
            assert!(truncated.width() <= width);
        }
        let header = DisplayText::truncate("/long-漢字漢字漢字/leaf", 17, Truncation::Middle);
        assert!(
            header.starts_with("/long-") && header.ends_with("/leaf"),
            "{header}"
        );
        assert!(header.contains('…') && header.width() <= 17, "{header}");
    }
}
