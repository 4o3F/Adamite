use crate::{McpError, SnapshotStore, response::Response};
use adamite_access::{JobError, JobStatus, ScanJob, ScanOptions};
use adamite_io::{driver::DriverProgress, status::ScanStatus};
use serde_json::json;
use std::{
    ffi::OsString,
    fs, io,
    path::{Path, PathBuf},
    sync::Mutex,
    thread,
    time::{Duration, Instant},
};

#[derive(Debug, PartialEq, Eq)]
enum ScanSource {
    Drive(char),
    Image(PathBuf),
}

impl ScanSource {
    fn parse(value: &str) -> Result<Self, McpError> {
        if let Some(letter) = SnapshotStore::volume(value) {
            return Ok(Self::Drive(letter));
        }
        let path = Path::new(value);
        Self::check_extension(path)?;
        let canonical =
            fs::canonicalize(path).map_err(|error| Self::invalid_image(value, error))?;
        Self::check_extension(&canonical)?;
        if !fs::metadata(&canonical)
            .map_err(|error| Self::invalid_image(value, error))?
            .is_file()
        {
            return Err(Self::invalid_image(value, "source is not a regular file"));
        }
        Ok(Self::Image(canonical))
    }

    fn check_extension(path: &Path) -> Result<(), McpError> {
        if path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("adm"))
        {
            return Err(McpError::UnsupportedScanSource {
                message: "source is already an .adm snapshot".into(),
                next: format!(
                    "Query it with get_tree and snapshot {:?}.",
                    path.to_string_lossy()
                ),
            });
        }
        Ok(())
    }

    fn invalid_image(value: &str, error: impl std::fmt::Display) -> McpError {
        McpError::InvalidArgument {
            message: format!("invalid volume {value:?}: {error}"),
            allowed:
                "Pass a single ASCII drive letter (Windows) or an existing regular NTFS image file."
                    .into(),
        }
    }

    fn matches(&self, value: &str) -> bool {
        match self {
            Self::Drive(letter) => SnapshotStore::volume(value) == Some(*letter),
            Self::Image(path) => fs::canonicalize(value)
                .or_else(|_| std::path::absolute(value))
                .is_ok_and(|candidate| candidate == *path),
        }
    }

    fn path(&self) -> PathBuf {
        match self {
            Self::Drive(letter) => format!("{letter}:").into(),
            Self::Image(path) => path.clone(),
        }
    }

    fn stem(&self) -> OsString {
        match self {
            Self::Drive(letter) => letter.to_string().into(),
            Self::Image(path) => path.file_stem().unwrap_or_default().to_owned(),
        }
    }
}

#[derive(Debug)]
enum State {
    Active(ScanJob),
    Done,
    Failed(JobError),
}

#[derive(Debug)]
struct ScanSession {
    source: ScanSource,
    target: PathBuf,
    status: PathBuf,
    started: Instant,
    progress: Option<DriverProgress>,
    state: State,
}

impl ScanSession {
    fn new(source: ScanSource, options: ScanOptions, directory: &Path) -> Result<Self, McpError> {
        let stem = source.stem();
        let mut snapshot = stem.clone();
        snapshot.push(".adm");
        let target = directory.join(snapshot);
        let mut status = OsString::from(".");
        status.push(stem);
        status.push(".status.jsonl");
        let status = directory.join(status);
        let job = ScanJob::scan_to(source.path(), options, target.clone(), status.clone())?;
        Ok(Self {
            source,
            target,
            status,
            started: Instant::now(),
            progress: None,
            state: State::Active(job),
        })
    }

    fn poll(&mut self) {
        if let State::Active(job) = &mut self.state {
            match job.poll() {
                JobStatus::Running(Some(progress)) => self.progress = Some(progress),
                JobStatus::Running(None) => {}
                // Retain only the outcome; ScanJob transfers its output exactly once.
                JobStatus::Done(_, _) => self.state = State::Done,
                JobStatus::Failed(error) => self.state = State::Failed(error),
            }
        }
    }

    fn running(&self, command: &str) -> Response {
        let (phase, records, planned, bytes) =
            match self.progress.as_ref().map(ScanStatus::from_progress) {
                Some(ScanStatus::Progress {
                    phase,
                    records,
                    planned,
                    bytes,
                }) => (phase, records, planned, bytes),
                _ => ("starting".into(), 0, None, 0),
            };
        let percentage = planned
            .filter(|planned| *planned > 0)
            .map(|planned| records as f64 / planned as f64 * 100.0);
        let elapsed = self.started.elapsed().as_secs_f64();
        Response::scan(
            format!(
                "running · {} → {} · phase {phase} · {}% · {bytes} bytes read · {elapsed:.1}s elapsed",
                self.source.path().display(),
                self.target.display(),
                percentage.map_or_else(|| "unknown".into(), |value| format!("{value:.1}"))
            ),
            json!({"command":command,"status":"running","volume":self.source.path(),"snapshot":{"path":self.target},"phase":phase,"percentage":percentage,"records_processed":records,"records_planned":planned,"bytes_read":bytes,"elapsed_seconds":elapsed}),
        )
    }

    fn response(&self) -> Result<Response, McpError> {
        match &self.state {
            State::Active(_) => Ok(self.running("scan_status")),
            State::Done => Ok(Response::scan(
                format!("done · {}", self.target.display()),
                json!({"command":"scan_status","status":"done","volume":self.source.path(),"snapshot":{"path":self.target}}),
            )),
            State::Failed(error) => Err(error.clone().into()),
        }
    }
}

/// Owns one active scan or its retained terminal outcome across server clones.
#[derive(Debug, Default)]
pub(crate) struct ScanJobs {
    current: Mutex<Option<ScanSession>>,
}

impl ScanJobs {
    /// Called on a blocking worker; the server serializes launch requests through UAC.
    pub(crate) fn start(
        &self,
        volume: &str,
        options: ScanOptions,
        directory: &Path,
    ) -> Result<Response, McpError> {
        {
            let mut current = self.current.lock().map_err(McpError::poisoned)?;
            if let Some(session) = &mut *current {
                session.poll();
                if matches!(session.state, State::Active(_)) {
                    return Err(McpError::ScanBusy);
                }
            }
            let source = ScanSource::parse(volume)?;
            if matches!(source, ScanSource::Drive(_)) && !cfg!(windows) {
                return Err(McpError::UnsupportedScanSource {
                    message: "drive scans require Windows".into(),
                    next: "Scan an NTFS image: adamite scan \"<image-file>\" -o \"<snapshot-dir>/image.adm\" --force".into(),
                });
            }

            if let Some(session) = &*current {
                // All worker and status-reader handles close before terminal publication.
                match fs::remove_file(&session.status) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(McpError::io(&session.status, error)),
                }
            }
            *current = Some(ScanSession::new(source, options, directory)?);
        }
        loop {
            {
                let mut current = self.current.lock().map_err(McpError::poisoned)?;
                let session = current.as_mut().ok_or_else(|| {
                    McpError::io(
                        directory,
                        io::Error::other("scan session disappeared during launch"),
                    )
                })?;
                match &session.state {
                    State::Active(job) if job.is_launching() => session.poll(),
                    State::Failed(error) => return Err(error.clone().into()),
                    _ => return Ok(session.running("scan_volume")),
                }
            }
            thread::sleep(Duration::from_millis(25));
        }
    }

    /// Polls active work or returns the retained outcome; volume mismatches are invalid arguments.
    pub(crate) fn status(&self, volume: Option<&str>) -> Result<Response, McpError> {
        let mut current = self.current.lock().map_err(McpError::poisoned)?;
        if let Some(session) = &mut *current {
            if volume.is_some_and(|value| !session.source.matches(value)) {
                return Err(McpError::InvalidArgument {
                    message: "volume does not match the current or last scan".into(),
                    allowed: format!("Omit volume or pass {:?}.", session.source.path()),
                });
            }
            session.poll();
            session.response()
        } else if volume.is_some() {
            Err(McpError::InvalidArgument {
                message: "no scan job has been started".into(),
                allowed: "Omit volume to read the idle status.".into(),
            })
        } else {
            Ok(Response::scan(
                "idle".into(),
                json!({"command":"scan_status","status":"idle"}),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::McpError;
    use adamite_access::{JobError, ScanJobError};

    #[test]
    fn declined_elevation_uses_typed_access_error() {
        let error = McpError::from(JobError::from(ScanJobError::ElevationDeclined));
        assert_eq!(error.code(), "elevation_declined");
        assert!(error.next_step().contains("Ask the user whether to retry"));
        let error = McpError::from(JobError::from(ScanJobError::ChildFailed {
            message: "elevation was declined".into(),
            exit_code: 4,
        }));
        assert_eq!(error.code(), "scan_failed");
        assert!(error.to_string().contains("exit code 4"));
    }
}
