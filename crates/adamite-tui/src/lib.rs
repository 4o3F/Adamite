//! Interactive, read-only directory browsing of an existing Adamite scan snapshot.
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
mod queries;
mod view;

#[cfg(test)]
mod tests;

use adamite_core::{scan::ScanOutput, snapshot::NodeId};
use app::{Action, App};
use ratatui::{DefaultTerminal, crossterm::event};
use snafu::Snafu;
use std::io;
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
        let result = self.run_loop(&mut terminal);
        let restored = ratatui::try_restore().map_err(|source| TuiError::Io {
            operation: "restore",
            source,
        });
        result.and(restored)
    }

    fn run_loop(self, terminal: &mut DefaultTerminal) -> Result<(), TuiError> {
        let area = terminal.size().map_err(|source| TuiError::Io {
            operation: "read size of",
            source,
        })?;
        let mut app = App::new(self.output, self.start, View::row_count(area.height));
        loop {
            terminal
                .draw(|frame| View::new(&app).render(frame))
                .map_err(|source| TuiError::Io {
                    operation: "draw",
                    source,
                })?;
            let event = event::read().map_err(|source| TuiError::Io {
                operation: "read input from",
                source,
            })?;
            if app.handle_terminal(event) == Action::Quit {
                return Ok(());
            }
        }
    }
}
