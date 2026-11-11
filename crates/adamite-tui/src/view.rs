use crate::app::{App, Cursor, Tab};
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
        if frame.area().width < 40 || frame.area().height < 8 {
            frame.render_widget(
                Paragraph::new("terminal too small (minimum 40×8)"),
                frame.area(),
            );
            return;
        }
        let [header, tabs, content, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(1),
        ])
        .areas(frame.area());
        let heading = if self.app.tab == Tab::Tree {
            self.path(self.app.tree.directory)
        } else {
            format!(
                "{} — scanned {}",
                self.app.tab.title(),
                ScanTime(self.app.snapshot().volume().context().scanned_at_filetime)
            )
        };
        frame.render_widget(
            Paragraph::new(DisplayText::truncate(
                &heading,
                usize::from(header.width),
                Truncation::Middle,
            )),
            header,
        );
        frame.render_widget(
            Tabs::new(Tab::ALL.map(Tab::title))
                .select(self.app.tab.index())
                .highlight_style(Style::new().fg(Color::Cyan)),
            tabs,
        );
        match self.app.tab {
            Tab::Tree => self.tree(frame, content),
            Tab::TopFiles => self.top_files(frame, content),
            Tab::Extensions => self.extensions(frame, content),
            Tab::Info => self.info(frame, content),
        }
        frame.render_widget(
            Paragraph::new("Tab/1–4: tabs  b: bytes  ?: help  q: quit"),
            footer,
        );
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

    fn row_style(selected: bool) -> Style {
        if selected {
            Style::new()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        }
    }

    fn top_files(&self, frame: &mut Frame<'_>, area: Rect) {
        let widths = [
            Constraint::Fill(1),
            Constraint::Length(self.size_width()),
            Constraint::Length(self.size_width()),
        ];
        let table = Rows {
            cursor: self.app.top_files,
            widths: &widths,
            headings: &["Path", "Allocated", "Logical"],
        };
        let path_width = table.name_width(area);
        let rows = table
            .visible(area, self.app.snapshot().top_files())
            .filter_map(|(index, &id)| {
                let node = self.app.snapshot().node(id)?;
                Some(
                    Row::new([
                        DisplayText::truncate(
                            &self.path(id),
                            usize::from(path_width),
                            Truncation::Middle,
                        ),
                        self.size(node.alloc()),
                        self.size(node.size()),
                    ])
                    .style(Self::row_style(index == table.cursor.selected)),
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
        let table = Rows {
            cursor: self.app.extensions,
            widths: &widths,
            headings: &["Extension", "Count", "Logical", "Allocated"],
        };
        let name_width = table.name_width(area);
        let rows = table
            .visible(area, self.app.snapshot().ext_stats())
            .map(|(index, stat)| {
                Row::new([
                    DisplayText::truncate(
                        &ExtensionLabel(stat.extension()).to_string(),
                        usize::from(name_width),
                        Truncation::End,
                    ),
                    stat.count().to_string(),
                    self.size(stat.total_size()),
                    self.size(stat.total_alloc()),
                ])
                .style(Self::row_style(index == table.cursor.selected))
            });
        table.render(frame, area, rows);
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
                Row::new([label.to_owned(), value])
                    .style(Self::row_style(index == table.cursor.selected))
            });
        table.render(frame, area, rows);
    }

    fn help(&self, frame: &mut Frame<'_>) {
        let area = frame
            .area()
            .centered(Constraint::Length(40), Constraint::Length(8));
        frame.render_widget(Clear, area);
        frame.render_widget(
            Paragraph::new(concat!(
                "↑/↓ k/j: move; PgUp/PgDn: page\n",
                "Home/End: first/last; Tab/1–4: tabs\n",
                "Enter/→/l: open; ←/h/Backspace: parent\n",
                "Top Enter: show file; b: raw bytes\n",
                "?/Esc: close help; q/Ctrl-C: quit\n",
                "Esc (help closed): quit",
            ))
            .block(Block::bordered().title("Help")),
            area,
        );
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

struct Rows<'a> {
    cursor: Cursor,
    widths: &'a [Constraint],
    headings: &'a [&'a str],
}

impl Rows<'_> {
    fn visible<T>(
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

    fn name_width(&self, area: Rect) -> u16 {
        Layout::horizontal(self.widths)
            .spacing(1)
            .split(area)
            .first()
            .map_or(0, |column| column.width)
    }

    fn render<'a>(
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
        let mut style = View::row_style(selected);
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

enum Truncation {
    End,
    Middle,
}

struct DisplayText;

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

    fn truncate(text: &str, width: usize, placement: Truncation) -> String {
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
