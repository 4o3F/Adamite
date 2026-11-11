//! Append-only JSON lines for scan progress and terminal outcomes.

use crate::driver::DriverProgress;
use adamite_core::scan::{ScanPhase, ScanProgress};
use serde::{Deserialize, Serialize};
use snafu::Snafu;
use std::{
    fs::{File, OpenOptions},
    io::{self, BufRead, BufReader, Write},
    path::Path,
    time::{Duration, Instant},
};

/// A progress report or terminal outcome, encoded as an externally tagged JSON object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanStatus {
    /// Scanner phase, record counts and cumulative logical bytes read.
    Progress {
        /// Stable snake_case phase name from the core scanner, or `unknown` for future phases.
        phase: String,
        /// Number of MFT records processed.
        records: u64,
        /// Total MFT record slots, when known.
        planned: Option<u64>,
        /// Cumulative logical bytes successfully read.
        bytes: u64,
    },
    /// Successful completion and the saved snapshot location.
    Done {
        /// Path of the saved snapshot.
        saved: String,
        /// Size of the saved snapshot in bytes.
        bytes: u64,
    },
    /// Failed completion and the exit code to report.
    Failed {
        /// User-facing error text.
        message: String,
        /// Child process exit code.
        exit_code: i32,
    },
}

impl ScanStatus {
    /// Stable wire names for core phases; later core phases are written as `unknown`.
    const PHASES: [(ScanPhase, &'static str); 11] = [
        (ScanPhase::Boot, "boot"),
        (ScanPhase::Record0, "record0"),
        (ScanPhase::Discover, "discover"),
        (ScanPhase::Stream, "stream"),
        (ScanPhase::Merge, "merge"),
        (ScanPhase::Link, "link"),
        (ScanPhase::Snapshot, "snapshot"),
        (ScanPhase::Aggregate, "aggregate"),
        (ScanPhase::Finalize, "finalize"),
        (ScanPhase::Done, "done"),
        (ScanPhase::Failed, "failed"),
    ];

    /// Converts driver counters into a progress report with a stable phase name.
    #[must_use]
    pub fn from_progress(progress: &DriverProgress) -> Self {
        let phase = Self::PHASES
            .iter()
            .find(|(phase, _)| *phase == progress.scan.phase)
            .map_or("unknown", |(_, name)| name);
        Self::Progress {
            phase: phase.to_owned(),
            records: progress.scan.records_processed,
            planned: progress.scan.records_planned,
            bytes: progress.bytes_read,
        }
    }

    /// Rebuilds driver counters from a progress report with a known phase name.
    ///
    /// Returns `None` for outcomes and unknown phases. Request-queue fields are not carried
    /// over the wire and read as idle.
    #[must_use]
    pub fn driver_progress(&self) -> Option<DriverProgress> {
        let Self::Progress {
            phase,
            records,
            planned,
            bytes,
        } = self
        else {
            return None;
        };
        let (phase, _) = Self::PHASES.iter().find(|(_, name)| name == phase)?;
        Some(DriverProgress {
            scan: ScanProgress {
                phase: *phase,
                records_processed: *records,
                records_planned: *planned,
                outstanding: 0,
                waiting_for_input: false,
            },
            bytes_read: *bytes,
        })
    }
}

/// Failure to read, write or decode a status line.
#[derive(Debug, Snafu)]
#[non_exhaustive]
pub enum StatusError {
    /// A host file read, write or flush failed.
    #[snafu(display("status file I/O failed: {source}"))]
    Io {
        /// Host file failure.
        source: io::Error,
    },
    /// A complete line contains invalid status JSON, or serialization failed.
    #[snafu(display("status JSON failed: {source}"))]
    Json {
        /// JSON encoding or decoding failure.
        source: serde_json::Error,
    },
}

/// Exclusively created, append-only status file with progress throttling.
#[derive(Debug)]
pub struct ScanStatusWriter {
    file: File,
    last_progress: Option<(ScanPhase, Instant)>,
}

impl ScanStatusWriter {
    /// Creates a new status file, failing if the path already exists.
    ///
    /// # Errors
    /// Returns a host error if exclusive creation fails.
    pub fn create(path: impl AsRef<Path>) -> io::Result<Self> {
        let file = OpenOptions::new()
            .append(true)
            .create_new(true)
            .open(path)?;
        Ok(Self {
            file,
            last_progress: None,
        })
    }

    /// Appends one complete JSON line and flushes it, without throttling.
    ///
    /// # Errors
    /// Returns [`StatusError`] if serialization, writing or flushing fails.
    pub fn write(&mut self, status: &ScanStatus) -> Result<(), StatusError> {
        let mut line = serde_json::to_vec(status).map_err(|source| StatusError::Json { source })?;
        line.push(b'\n');
        self.file
            .write_all(&line)
            .and_then(|()| self.file.flush())
            .map_err(|source| StatusError::Io { source })
    }

    /// Writes the first report, each phase change, and reports at least 500 ms apart otherwise.
    ///
    /// `now` is supplied by the caller so throttling can be tested without sleeping.
    ///
    /// # Errors
    /// Returns [`StatusError`] if the selected report cannot be written and flushed.
    pub fn progress(&mut self, progress: &DriverProgress, now: Instant) -> Result<(), StatusError> {
        const INTERVAL: Duration = Duration::from_millis(500);

        if let Some((phase, last_write)) = self.last_progress
            && phase == progress.scan.phase
            && now.saturating_duration_since(last_write) < INTERVAL
        {
            return Ok(());
        }
        self.write(&ScanStatus::from_progress(progress))?;
        self.last_progress = Some((progress.scan.phase, now));
        Ok(())
    }
}

/// Incremental status reader that retains incomplete trailing lines between polls.
#[derive(Debug)]
pub struct ScanStatusReader {
    file: BufReader<File>,
    pending: Vec<u8>,
}

impl ScanStatusReader {
    /// Opens an existing status file from its beginning.
    ///
    /// # Errors
    /// Returns a host error if the file cannot be opened.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        Ok(Self {
            file: BufReader::new(File::open(path)?),
            pending: Vec::new(),
        })
    }

    /// Returns new newline-terminated status lines, retaining a partial trailing line.
    ///
    /// A malformed complete line is consumed when its decoding error is returned.
    ///
    /// # Errors
    /// Returns [`StatusError::Io`] on read failure or [`StatusError::Json`] for invalid JSON.
    pub fn poll(&mut self) -> Result<Vec<ScanStatus>, StatusError> {
        let mut statuses = Vec::new();
        loop {
            let read = self
                .file
                .read_until(b'\n', &mut self.pending)
                .map_err(|source| StatusError::Io { source })?;
            if read == 0 || self.pending.last() != Some(&b'\n') {
                break;
            }
            let status = serde_json::from_slice(&self.pending);
            self.pending.clear();
            statuses.push(status.map_err(|source| StatusError::Json { source })?);
        }
        Ok(statuses)
    }
}
