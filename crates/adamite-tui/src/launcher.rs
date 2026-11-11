//! Source selection and job driving without source I/O or worker ownership policies.

use crate::{
    TuiError,
    app::{Action, App, Cursor},
    browser_loop,
    input::{Event, InputLine, Key},
    launcher_view, read_event, terminal_rows,
    view::View,
    with_terminal,
};
use adamite_core::{scan::ScanOutput, snapshot::NodeId};
use ratatui::{Terminal, backend::Backend, crossterm::event};
use std::{
    fmt,
    time::{Duration, Instant, SystemTime},
};

/// A saved snapshot's display information; the source provider owns path interpretation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotInfo {
    /// File stem or other short display name.
    pub name: String,
    /// Path for display and for the provider to identify the snapshot.
    pub path: String,
    /// Last modification time, when available.
    pub modified: Option<SystemTime>,
    /// A listing or metadata error to show on this row.
    pub error: Option<String>,
}

/// A source row, in the order supplied by [`Sources::entries`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    /// A Windows NTFS drive, optionally accompanied by its saved snapshot.
    Drive {
        /// Drive letter without a colon.
        letter: char,
        /// Volume label, which may be empty.
        label: String,
        /// Total capacity in bytes.
        capacity: u64,
        /// Saved snapshot to open instead of scanning on Enter.
        snapshot: Option<SnapshotInfo>,
    },
    /// A saved snapshot not already represented by a drive row.
    Snapshot(SnapshotInfo),
}

/// Supplies home rows and starts jobs. Implementations perform all source I/O.
///
/// Starting a job should return promptly; perform long work on a worker or in a child process.
/// Loading an existing snapshot also returns a job, even when it completes on its first poll.
pub trait Sources {
    /// Lists drives and snapshots, including any per-row errors.
    fn entries(&mut self) -> Vec<Entry>;
    /// Opens a row, or scans a drive when it has no snapshot or `rescan` is true.
    fn start(&mut self, entry: &Entry, rescan: bool) -> Box<dyn ScanJob>;
    /// Opens a typed image or snapshot path, scanning an image as needed.
    fn open_path(&mut self, path: &str) -> Box<dyn ScanJob>;
}

/// A job's latest counters, converted by the provider into TUI-owned display data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobProgress {
    /// Human-readable scan phase.
    pub phase: String,
    /// Records processed so far.
    pub records: u64,
    /// Expected record count, when known.
    pub planned: Option<u64>,
    /// Cumulative bytes read.
    pub bytes: u64,
}

/// A nonblocking job poll result.
#[derive(Debug)]
pub enum JobState {
    /// Work continues; absent progress retains any previously displayed counters.
    Running {
        /// Title describing the source or operation.
        label: String,
        /// New counters since the preceding poll, when available.
        progress: Option<JobProgress>,
    },
    /// Transfers the completed scan into the launcher for browsing.
    Done(ScanOutput),
    /// Returns to Home with this status message.
    Failed(String),
}

/// A source operation whose lifetime and cancellation behavior are owned by its provider.
pub trait ScanJob {
    /// Reports progress or a terminal outcome without waiting for work to finish.
    fn poll(&mut self) -> JobState;
}

/// Takes over the terminal to select sources, display job progress and browse results.
#[derive(Debug)]
pub struct Launcher<S: Sources> {
    pub(crate) sources: S,
    pub(crate) home: Home,
    pub(crate) active: Option<ActiveJob>,
}

#[derive(Debug)]
pub(crate) struct Home {
    pub(crate) entries: Vec<Entry>,
    pub(crate) cursor: Cursor,
    pub(crate) input: Option<InputLine<&'static str>>,
    pub(crate) help: bool,
    pub(crate) status: Option<String>,
    page_rows: usize,
}

pub(crate) struct ActiveJob {
    job: Box<dyn ScanJob>,
    pub(crate) label: String,
    pub(crate) progress: Option<JobProgress>,
    pub(crate) rate: u128,
    sample: (Instant, u64),
}

impl fmt::Debug for ActiveJob {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ActiveJob")
            .field("label", &self.label)
            .field("progress", &self.progress)
            .field("rate", &self.rate)
            .finish_non_exhaustive()
    }
}

impl Home {
    fn visible_rows(&self) -> usize {
        self.page_rows
            .saturating_sub(usize::from(self.input.is_some()))
            .max(1)
    }

    fn resize(&mut self, page_rows: usize) {
        self.page_rows = page_rows.max(1);
        self.cursor.move_to(
            self.cursor.selected,
            self.entries.len().saturating_add(1),
            self.visible_rows(),
        );
    }
}

impl<S: Sources> Launcher<S> {
    /// Lists initial sources. The fixed `Open file…` row is appended by the TUI.
    #[must_use]
    pub fn new(mut sources: S) -> Self {
        let entries = sources.entries();
        Self {
            sources,
            home: Home {
                entries,
                cursor: Cursor::default(),
                input: None,
                help: false,
                status: None,
                page_rows: 1,
            },
            active: None,
        }
    }

    /// Runs Home, progress and browsing until quit, restoring the terminal afterward.
    ///
    /// `o` returns from browsing to a refreshed Home. Quitting drops the active job;
    /// its provider decides whether the underlying work continues.
    ///
    /// # Errors
    /// Returns terminal initialization, drawing, input or restoration failures, with
    /// the same restoration discipline as [`crate::Browser::run`].
    pub fn run(mut self) -> Result<(), TuiError> {
        with_terminal(|terminal| self.run_loop(terminal, &mut read_event))
    }

    pub(crate) fn run_loop<B: Backend>(
        &mut self,
        terminal: &mut Terminal<B>,
        next_event: &mut impl FnMut(Option<Duration>) -> Result<Option<event::Event>, TuiError>,
    ) -> Result<(), TuiError>
    where
        B::Error: Send + Sync + 'static,
    {
        self.home.resize(terminal_rows(terminal)?);
        loop {
            if let Some(output) = self.poll(Instant::now()) {
                let mut app = App::new(&output, NodeId::ROOT, terminal_rows(terminal)?);
                app.return_home = true;
                if browser_loop(terminal, app, next_event)? == Action::Quit {
                    return Ok(());
                }
                // The browsing App has been dropped; release its owned output before listing again.
                drop(output);
                self.return_home(None);
                self.home.resize(terminal_rows(terminal)?);
            }
            terminal
                .draw(|frame| launcher_view::render(self, frame, SystemTime::now()))
                .map_err(|source| TuiError::Io {
                    operation: "draw",
                    source: std::io::Error::other(source),
                })?;
            let timeout = self.active.as_ref().map(|_| Duration::from_millis(100));
            let Some(event) = next_event(timeout)? else {
                continue;
            };
            if self.handle_terminal(event, Instant::now()) == Action::Quit {
                return Ok(());
            }
        }
    }

    pub(crate) fn return_home(&mut self, status: Option<String>) {
        self.active = None;
        self.home.entries = self.sources.entries();
        self.home.input = None;
        self.home.help = false;
        self.home.status = status;
        self.home.resize(self.home.page_rows);
    }

    pub(crate) fn poll(&mut self, now: Instant) -> Option<ScanOutput> {
        let active = self.active.as_mut()?;
        match active.job.poll() {
            JobState::Running { label, progress } => {
                active.label = label;
                if let Some(progress) = progress {
                    let nanos = now
                        .checked_duration_since(active.sample.0)
                        .unwrap_or_default()
                        .as_nanos();
                    active.rate = u128::from(progress.bytes.saturating_sub(active.sample.1))
                        .checked_mul(1_000_000_000)
                        .and_then(|bytes| bytes.checked_div(nanos))
                        .unwrap_or_default();
                    active.sample = (now, progress.bytes);
                    active.progress = Some(progress);
                }
                None
            }
            JobState::Done(output) => {
                self.active = None;
                Some(output)
            }
            JobState::Failed(message) => {
                self.return_home(Some(message));
                None
            }
        }
    }

    pub(crate) fn handle_terminal(&mut self, event: event::Event, now: Instant) -> Action {
        if self.active.is_none()
            && let Some(input) = &mut self.home.input
            && matches!(event, event::Event::Key(_))
        {
            return input
                .handle(&event)
                .map_or(Action::Continue, |key| self.handle(Event::Key(key), now));
        }
        Event::from_terminal(event).map_or(Action::Continue, |event| self.handle(event, now))
    }

    fn handle(&mut self, event: Event, now: Instant) -> Action {
        let key = match event {
            Event::Resize { height, .. } => {
                self.home.resize(View::row_count(height));
                return Action::Continue;
            }
            Event::Key(key) => key,
        };
        if self.active.is_some() {
            if key == Key::Quit {
                self.active = None;
                return Action::Quit;
            }
            return Action::Continue;
        }
        if self.home.input.is_some() {
            match key {
                Key::Escape => self.home.input = None,
                Key::Enter => {
                    if let Some(input) = self.home.input.take() {
                        let job = self.sources.open_path(input.editor.value());
                        self.begin(job, now);
                    }
                }
                _ => {}
            }
            self.home.resize(self.home.page_rows);
            return Action::Continue;
        }
        match key {
            Key::Quit => return Action::Quit,
            Key::Help | Key::Escape if self.home.help || key == Key::Help => {
                self.home.help = !self.home.help
            }
            _ if self.home.help => {}
            Key::Enter => {
                if let Some(entry) = self.home.entries.get(self.home.cursor.selected) {
                    let job = self.sources.start(entry, false);
                    self.begin(job, now);
                } else {
                    self.home.input = Some(InputLine::new("Open file: ", String::new()));
                    self.home.status = None;
                    self.home.resize(self.home.page_rows);
                }
            }
            Key::Rescan => {
                if let Some(entry @ Entry::Drive { .. }) =
                    self.home.entries.get(self.home.cursor.selected)
                {
                    let job = self.sources.start(entry, true);
                    self.begin(job, now);
                }
            }
            _ => self.home.cursor.handle(
                key,
                self.home.entries.len().saturating_add(1),
                self.home.visible_rows(),
            ),
        }
        Action::Continue
    }

    fn begin(&mut self, job: Box<dyn ScanJob>, now: Instant) {
        self.home.help = false;
        self.home.status = None;
        self.active = Some(ActiveJob {
            job,
            label: "Loading…".to_owned(),
            progress: None,
            rate: 0,
            sample: (now, 0),
        });
    }
}
