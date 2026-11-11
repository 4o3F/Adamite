use crate::{
    launcher::{ActiveJob, Entry, Launcher, SnapshotInfo, Sources},
    view::{DisplayText, Rows, Truncation, render_help, render_input, row_style, too_small},
};
use adamite_format::{Age, ByteSize, Escaped};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Style},
    widgets::{Gauge, Paragraph, Row},
};
use std::time::SystemTime;

pub(crate) fn render<S: Sources>(launcher: &Launcher<S>, frame: &mut Frame<'_>, now: SystemTime) {
    if too_small(frame) {
        return;
    }
    if let Some(active) = &launcher.active {
        progress(active, frame);
        return;
    }
    let home = &launcher.home;
    let [header, hint, content, prompt, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(u16::from(home.input.is_some())),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    frame.render_widget(Paragraph::new("Adamite — Home"), header);
    if home.entries.is_empty() {
        frame.render_widget(
            Paragraph::new("No saved snapshots. Open an image or snapshot file."),
            hint,
        );
    }
    let table = Rows {
        cursor: home.cursor,
        widths: &[Constraint::Percentage(55), Constraint::Fill(1)],
        headings: &["Source", "Snapshot / modified"],
    };
    let name_width = usize::from(table.name_width(content));
    let rows = table
        .visible(
            content,
            home.entries.iter().map(Some).chain(std::iter::once(None)),
        )
        .map(|(index, entry)| {
            let (name, detail) = match entry {
                Some(Entry::Drive {
                    letter,
                    label,
                    capacity,
                    snapshot,
                }) => (
                    format!(
                        "{letter}:  {}  {}",
                        Escaped(label),
                        size(u128::from(*capacity))
                    ),
                    snapshot.as_ref().map_or_else(
                        || "not scanned".to_owned(),
                        |info| format!("snapshot {}", snapshot_detail(info, now)),
                    ),
                ),
                Some(Entry::Snapshot(info)) => {
                    (Escaped(&info.name).to_string(), snapshot_detail(info, now))
                }
                None => ("Open file…".to_owned(), String::new()),
            };
            Row::new([
                DisplayText::truncate(&name, name_width, Truncation::End),
                detail,
            ])
            .style(row_style(index == home.cursor.selected))
        });
    table.render(frame, content, rows);
    if let Some(input) = &home.input {
        render_input(frame, prompt, input);
    }
    let status = home.status.as_ref().map_or_else(
        || {
            if home.input.is_some() {
                "Enter: open  Esc: cancel  ←/→ Home/End: edit".to_owned()
            } else {
                "Enter: open  r: rescan drive  ?: help  q: quit".to_owned()
            }
        },
        |status| Escaped(status).to_string(),
    );
    frame.render_widget(Paragraph::new(status), footer);
    if home.help {
        render_help(
            frame,
            concat!(
                "↑/↓ k/j; PgUp/PgDn; Home/End\n",
                "Enter: open snapshot or scan drive\n",
                "r: rescan selected drive\n",
                "Open file…: type image/snapshot path\n",
                "q/Ctrl-C: quit; ?/Esc: close help\n",
                "Input: Enter opens; Esc cancels",
            ),
        );
    }
}

fn snapshot_detail(info: &SnapshotInfo, now: SystemTime) -> String {
    info.error.as_ref().map_or_else(
        || Age::new(info.modified, now).to_string(),
        |error| Escaped(error).to_string(),
    )
}

fn size(value: u128) -> String {
    ByteSize {
        value,
        raw_bytes: false,
    }
    .to_string()
}

fn progress(active: &ActiveJob, frame: &mut Frame<'_>) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    frame.render_widget(Paragraph::new(Escaped(&active.label).to_string()), header);
    let [phase, gauge, bytes, rate] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(body);
    if let Some(progress) = &active.progress {
        frame.render_widget(Paragraph::new(Escaped(&progress.phase).to_string()), phase);
        match progress.planned {
            Some(planned) => {
                let percent = u128::from(progress.records)
                    .checked_mul(100)
                    .and_then(|records| records.checked_div(u128::from(planned)))
                    .unwrap_or_default()
                    .min(100);
                frame.render_widget(
                    Gauge::default()
                        .gauge_style(Style::new().fg(Color::Cyan))
                        .percent(u16::try_from(percent).unwrap_or_default())
                        .label(format!("{} / {planned} records", progress.records)),
                    gauge,
                );
            }
            None => indeterminate(
                frame,
                gauge,
                &format!("{} records · total unknown", progress.records),
            ),
        }
        frame.render_widget(
            Paragraph::new(format!("Bytes read: {}", size(u128::from(progress.bytes)))),
            bytes,
        );
        frame.render_widget(
            Paragraph::new(format!("Rate: {}/s", size(active.rate))),
            rate,
        );
    } else {
        indeterminate(frame, gauge, "Working… · total unknown");
    }
    frame.render_widget(Paragraph::new("q: quit"), footer);
}

fn indeterminate(frame: &mut Frame<'_>, area: Rect, text: &str) {
    frame.render_widget(Paragraph::new(text), area);
}
