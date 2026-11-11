//! Source selection, job progress and read-only browsing of Adamite scan snapshots.
#![forbid(unsafe_code)]
#![deny(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

mod app;
mod input;
mod launcher;
mod launcher_view;

pub use launcher::{Entry, JobProgress, JobState, Launcher, ScanJob, SnapshotInfo, Sources};
mod queries;
mod view;

#[cfg(test)]
mod tests;

use adamite_core::{scan::ScanOutput, snapshot::NodeId};
use app::{Action, App};
use ratatui::{DefaultTerminal, Terminal, backend::Backend, crossterm::event};
use snafu::Snafu;
use std::{io, time::Duration};
use view::View;

/// A terminal I/O failure while setting up, running or restoring the browser.
#[derive(Debug, Snafu)]
#[non_exhaustive]
pub enum TuiError {
    /// The terminal could not complete the named operation.
    #[snafu(display("cannot {operation} terminal"))]
    Io {
        /// The operation that failed.
        operation: &'static str,
        /// The underlying terminal I/O error.
        source: io::Error,
    },
}

/// Borrows a completed scan for interactive browsing without changing the snapshot.
#[derive(Debug)]
pub struct Browser<'a> {
    output: &'a ScanOutput,
    start: NodeId,
}

impl<'a> Browser<'a> {
    /// Starts at the supplied directory, falling back to root for an invalid or file identifier.
    #[must_use]
    pub fn new(output: &'a ScanOutput, start: NodeId) -> Self {
        Self { output, start }
    }

    /// Takes over the terminal until the user quits and then restores it.
    ///
    /// # Errors
    ///
    /// Returns terminal initialization, drawing, input or restoration failures. Restoration is
    /// attempted after initialization fails and after every event-loop result; a loop failure
    /// takes precedence over a restoration failure. Ratatui's restoration panic hook is retained.
    pub fn run(self) -> Result<(), TuiError> {
        with_terminal(|terminal| {
            let app = App::new(self.output, self.start, terminal_rows(terminal)?);
            browser_loop(terminal, app, &mut read_event).map(|_| ())
        })
    }
}

fn terminal_rows<B: Backend>(terminal: &Terminal<B>) -> Result<usize, TuiError>
where
    B::Error: Send + Sync + 'static,
{
    terminal
        .size()
        .map(|area| View::row_count(area.height))
        .map_err(|source| TuiError::Io {
            operation: "read size of",
            source: io::Error::other(source),
        })
}

fn browser_loop<B: Backend>(
    terminal: &mut Terminal<B>,
    mut app: App<'_>,
    next_event: &mut impl FnMut(Option<Duration>) -> Result<Option<event::Event>, TuiError>,
) -> Result<Action, TuiError>
where
    B::Error: Send + Sync + 'static,
{
    loop {
        terminal
            .draw(|frame| View::new(&app).render(frame))
            .map_err(|source| TuiError::Io {
                operation: "draw",
                source: io::Error::other(source),
            })?;
        let Some(event) = next_event(None)? else {
            continue;
        };
        let action = app.handle_terminal(event);
        if action != Action::Continue {
            return Ok(action);
        }
    }
}

fn read_event(timeout: Option<Duration>) -> Result<Option<event::Event>, TuiError> {
    if let Some(timeout) = timeout
        && !event::poll(timeout).map_err(|source| TuiError::Io {
            operation: "poll input from",
            source,
        })?
    {
        return Ok(None);
    }
    event::read().map(Some).map_err(|source| TuiError::Io {
        operation: "read input from",
        source,
    })
}

fn with_terminal(
    run: impl FnOnce(&mut DefaultTerminal) -> Result<(), TuiError>,
) -> Result<(), TuiError> {
    let startup = tracing::info_span!("tui_startup").entered();
    let mut terminal = match ratatui::try_init() {
        Ok(terminal) => terminal,
        Err(source) => {
            let _ = ratatui::try_restore();
            return Err(TuiError::Io {
                operation: "initialize",
                source,
            });
        }
    };
    drop(startup);
    let result = run(&mut terminal);
    let restored = ratatui::try_restore().map_err(|source| TuiError::Io {
        operation: "restore",
        source,
    });
    result.and(restored)
}
