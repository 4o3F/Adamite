use crate::app::App;
use adamite_core::snapshot::{NodeFlags, NodeView};
use adamite_format::{ByteSize, Escaped};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    widgets::{Cell, Paragraph, Row, Table, Tabs},
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
        let [header, tabs, tree, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(1),
        ])
        .areas(frame.area());
        let path = self
            .app
            .snapshot()
            .path(self.app.tree.directory)
            .ok()
            .flatten()
            .unwrap_or_else(|| format!("#{}", self.app.tree.directory.raw()));
        let path = Escaped(&path).to_string();
        frame.render_widget(
            Paragraph::new(DisplayText::truncate(
                &path,
                usize::from(header.width),
                Truncation::Middle,
            )),
            header,
        );
        frame.render_widget(
            Tabs::new(["Tree"]).highlight_style(Style::new().fg(Color::Cyan)),
            tabs,
        );
        self.tree(frame, tree);
        frame.render_widget(
            Paragraph::new("↑/↓ j/k  Enter: open  ←/h: parent  q: quit"),
            footer,
        );
    }

    fn tree(&self, frame: &mut Frame<'_>, area: Rect) {
        let widths = [
            Constraint::Fill(1),
            Constraint::Length(11),
            Constraint::Length(20),
            Constraint::Length(11),
            Constraint::Length(10),
        ];
        let columns = Layout::horizontal(widths).spacing(1).split(area);
        let name_width = columns.first().map_or(0, |column| column.width);
        let parent_alloc = self
            .app
            .snapshot()
            .node(self.app.tree.directory)
            .map_or(0, |node| node.total_alloc());
        let visible = usize::from(area.height.saturating_sub(1));
        let rows = self
            .app
            .children()
            .iter()
            .enumerate()
            .skip(self.app.tree.cursor.scroll)
            .take(visible)
            .filter_map(|(index, &id)| {
                let node = self.app.snapshot().node(id)?;
                let name = self.app.snapshot().name(id)?;
                Some(TreeRow { node, name }.render(
                    name_width,
                    parent_alloc,
                    index == self.app.tree.cursor.selected,
                ))
            });
        let table = Table::new(rows, widths).column_spacing(1).header(
            Row::new(["Name", "Allocated", "Share of parent", "Logical", "Files"])
                .style(Style::new().add_modifier(Modifier::BOLD)),
        );
        frame.render_widget(table, area);
        if self.app.children().is_empty() {
            let [_, body] =
                Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
            frame.render_widget(Paragraph::new("(empty)"), body);
        }
    }
}

struct TreeRow<'a> {
    node: NodeView<'a>,
    name: &'a str,
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
        let mut style = Style::default();
        if flags.contains(NodeFlags::SYNTHETIC) {
            style = style.add_modifier(Modifier::DIM);
        }
        if selected {
            style = style.bg(Color::DarkGray).add_modifier(Modifier::BOLD);
        }
        Row::new([
            Cell::from(name),
            Cell::from(
                ByteSize {
                    value: u128::from(self.node.total_alloc()),
                    raw_bytes: false,
                }
                .to_string(),
            ),
            Cell::from(share),
            Cell::from(
                ByteSize {
                    value: u128::from(self.node.total_size()),
                    raw_bytes: false,
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
