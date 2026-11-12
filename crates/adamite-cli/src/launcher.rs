//! CLI source I/O behind the TUI's home and nonblocking job interfaces.

use crate::{
    cli::LimitArgs,
    elevated_scan::{ElevatedScan, JobState as ElevatedState},
    error::CliError,
    save::{PublishMode, SnapshotFile},
    scan_progress_view::ScanProgressView,
    source::SourceExt,
};
use adamite_access::Source;
use adamite_core::scan::ScanOutput;
use adamite_io::{driver::DriverProgress, snapshot_dir::SnapshotDir};
use adamite_tui::{Entry, JobProgress, JobState, ScanJob, SnapshotInfo, Sources};
use std::{
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, SyncSender, TryRecvError},
    thread,
};

pub(crate) struct CliSources {
    limits: LimitArgs,
    directory: Option<SnapshotDir>,
    // Keep native paths alongside the display strings returned to the TUI.
    paths: Vec<(String, PathBuf)>,
}

impl CliSources {
    pub(crate) fn new(limits: LimitArgs) -> Self {
        Self::with_dir(
            limits,
            SnapshotDir::default_path().ok().map(SnapshotDir::new),
        )
    }

    pub(crate) fn with_dir(limits: LimitArgs, directory: Option<SnapshotDir>) -> Self {
        Self {
            limits,
            directory,
            paths: Vec::new(),
        }
    }

    fn snapshots(&mut self) -> Vec<Entry> {
        self.paths.clear();
        let entries = self
            .directory
            .as_ref()
            .and_then(|directory| directory.entries().ok())
            .unwrap_or_default();
        entries
            .into_iter()
            .map(|entry| {
                let path = entry
                    .path()
                    .map(|path| path.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if let Some(native) = entry.path() {
                    self.paths.push((path.clone(), native.to_owned()));
                }
                Entry::Snapshot(SnapshotInfo {
                    name: entry
                        .file_stem()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "unreadable snapshot".to_owned()),
                    path,
                    modified: entry.modified(),
                    error: entry.error().map(ToString::to_string),
                })
            })
            .collect()
    }

    fn snapshot_path(&self, info: &SnapshotInfo) -> PathBuf {
        self.paths
            .iter()
            .find(|(display, _)| display == &info.path)
            .map_or_else(|| PathBuf::from(&info.path), |(_, native)| native.clone())
    }

    fn scan_drive(&self, letter: char) -> Result<Box<dyn ScanJob>, String> {
        let directory = self
            .directory
            .as_ref()
            .ok_or("cannot locate snapshot directory")?;
        directory.create().map_err(|error| error.to_string())?;
        let letter = letter.to_ascii_uppercase();
        let path = PathBuf::from(format!("{letter}:"));
        let destination = directory.drive_snapshot(letter);
        let source = Source::open(&path).map_err(|error| CliError::from(error).message())?;
        let file = SnapshotFile::new(destination.clone(), PublishMode::Replace);
        file.preflight(&source).map_err(|error| error.message())?;
        let limits = self.limits.clone();
        let label = format!("Scanning {}", path.display());
        if let Some(scan) = source
            .elevation(&limits, Some((&destination, PublishMode::Replace)))
            .map_err(|error| error.message())?
        {
            let job = ElevatedWorker::spawn(scan, path, limits.clone())?;
            return Ok(Box::new(DriveJob {
                state: DriveState::Scanning(job),
                limits,
                label,
            }));
        }
        Ok(WorkerJob::spawn(label, move |progress| {
            let output = source.load_observed_cli(&limits, |report| {
                let _ = progress.send(report);
            })?;
            file.save(&output)?;
            Ok(output)
        }))
    }
}

impl Sources for CliSources {
    fn entries(&mut self) -> Vec<Entry> {
        let snapshots = self.snapshots();
        #[cfg(windows)]
        {
            use adamite_io::volume::Volume;
            let mut snapshots = snapshots;
            let mut entries = Vec::new();
            for volume in Volume::list().unwrap_or_default() {
                let snapshot = snapshots
                    .iter()
                    .position(|entry| match entry {
                        Entry::Snapshot(info) => {
                            info.name.eq_ignore_ascii_case(&volume.letter().to_string())
                        }
                        Entry::Drive { .. } => false,
                    })
                    .and_then(|index| match snapshots.remove(index) {
                        Entry::Snapshot(info) => Some(info),
                        Entry::Drive { .. } => None,
                    });
                entries.push(Entry::Drive {
                    letter: volume.letter(),
                    label: volume.label().to_owned(),
                    capacity: volume.capacity(),
                    snapshot,
                });
            }
            entries.extend(snapshots);
            entries
        }
        #[cfg(not(windows))]
        {
            snapshots
        }
    }

    fn start(&mut self, entry: &Entry, rescan: bool) -> Box<dyn ScanJob> {
        match entry {
            Entry::Drive {
                snapshot: Some(info),
                ..
            } if !rescan => WorkerJob::load(self.snapshot_path(info), self.limits.clone()),
            Entry::Drive { letter, .. } => self.scan_drive(*letter).unwrap_or_else(failed),
            Entry::Snapshot(info) => WorkerJob::load(self.snapshot_path(info), self.limits.clone()),
        }
    }

    fn open_path(&mut self, path: &str) -> Box<dyn ScanJob> {
        let path = Path::new(path);
        match Source::open(path) {
            Ok(Source::Drive { letter, .. }) => {
                let row = self.entries().into_iter().find(|entry| matches!(entry, Entry::Drive { letter: listed, .. } if listed.eq_ignore_ascii_case(&letter)));
                match row {
                    Some(entry) => self.start(&entry, false),
                    None => self.scan_drive(letter).unwrap_or_else(failed),
                }
            }
            Ok(source) => {
                let label = if source.is_snapshot() {
                    format!("Loading {}", path.display())
                } else {
                    format!("Scanning {}", path.display())
                };
                let limits = self.limits.clone();
                WorkerJob::spawn(label, move |progress| {
                    source.load_observed_cli(&limits, |report| {
                        let _ = progress.send(report);
                    })
                })
            }
            Err(error) => failed(CliError::from(error).message()),
        }
    }
}

fn progress(report: DriverProgress) -> JobProgress {
    JobProgress {
        phase: ScanProgressView::phase_label(report.scan.phase).to_owned(),
        records: report.scan.records_processed,
        planned: report.scan.records_planned,
        bytes: report.bytes_read,
    }
}

struct FailedJob(String);

fn failed(message: String) -> Box<dyn ScanJob> {
    Box::new(FailedJob(message))
}

impl ScanJob for FailedJob {
    fn poll(&mut self) -> JobState {
        JobState::Failed(self.0.clone())
    }
}

struct WorkerJob {
    label: String,
    progress: Receiver<DriverProgress>,
    result: Receiver<Result<ScanOutput, CliError>>,
}

impl WorkerJob {
    fn load(path: PathBuf, limits: LimitArgs) -> Box<dyn ScanJob> {
        Self::spawn(format!("Loading {}", path.display()), move |_| {
            Source::open(&path)?.load_observed_cli(&limits, |_| {})
        })
    }

    fn spawn(
        label: String,
        work: impl FnOnce(mpsc::Sender<DriverProgress>) -> Result<ScanOutput, CliError> + Send + 'static,
    ) -> Box<dyn ScanJob> {
        let (progress_sender, progress) = mpsc::channel();
        let (result_sender, result) = mpsc::channel();
        match thread::Builder::new()
            .name("adamite-scan".to_owned())
            .spawn(move || {
                let output = work(progress_sender);
                let _ = result_sender.send(output);
            }) {
            Ok(worker) => {
                drop(worker); // Jobs detach their workers; process exit ends in-process work.
                Box::new(Self {
                    label,
                    progress,
                    result,
                })
            }
            Err(error) => failed(format!("cannot start scan worker: {error}")),
        }
    }
}

impl ScanJob for WorkerJob {
    fn poll(&mut self) -> JobState {
        let latest = self.progress.try_iter().last().map(progress);
        match self.result.try_recv() {
            Ok(Ok(output)) => JobState::Done(output),
            Ok(Err(error)) => JobState::Failed(error.message()),
            Err(TryRecvError::Disconnected) => {
                JobState::Failed("scan worker stopped without a result".to_owned())
            }
            Err(TryRecvError::Empty) => JobState::Running {
                label: self.label.clone(),
                progress: latest,
            },
        }
    }
}

/// The UAC prompt and its process handle stay on the same worker thread. UI polls
/// request one job poll at a time, so opening the prompt cannot block terminal input.
struct ElevatedWorker {
    requests: SyncSender<()>,
    responses: Receiver<Result<ElevatedState, String>>,
}

impl ElevatedWorker {
    fn spawn(scan: ElevatedScan, path: PathBuf, limits: LimitArgs) -> Result<Self, String> {
        let (requests, incoming) = mpsc::sync_channel(1);
        let (outgoing, responses) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("adamite-elevation".to_owned())
            .spawn(move || {
                let mut job = match scan.launch(&path, &limits) {
                    Ok(job) => job,
                    Err(error) => {
                        let _ = outgoing.send(Err(error.message()));
                        return;
                    }
                };
                while incoming.recv().is_ok() {
                    let state = job.poll().map_err(|error| error.message());
                    let terminal = !matches!(state, Ok(ElevatedState::Running(_)));
                    if outgoing.send(state).is_err() || terminal {
                        break;
                    }
                }
            })
            .map_err(|error| format!("cannot start elevation worker: {error}"))?;
        drop(worker);
        Ok(Self {
            requests,
            responses,
        })
    }

    fn poll(&mut self) -> Result<ElevatedState, String> {
        match self.responses.try_recv() {
            Ok(state) => {
                if matches!(state, Ok(ElevatedState::Running(_))) {
                    let _ = self.requests.try_send(());
                }
                state
            }
            Err(TryRecvError::Empty) => {
                let _ = self.requests.try_send(());
                Ok(ElevatedState::Running(None))
            }
            Err(TryRecvError::Disconnected) => {
                Err("elevated scan worker stopped without a result".to_owned())
            }
        }
    }
}

enum DriveState {
    Scanning(ElevatedWorker),
    Loading(Box<dyn ScanJob>),
}

struct DriveJob {
    state: DriveState,
    limits: LimitArgs,
    label: String,
}

impl ScanJob for DriveJob {
    fn poll(&mut self) -> JobState {
        let job = match &mut self.state {
            DriveState::Loading(loading) => return loading.poll(),
            DriveState::Scanning(job) => job,
        };
        match job.poll() {
            Ok(ElevatedState::Running(latest)) => JobState::Running {
                label: self.label.clone(),
                progress: latest.map(progress),
            },
            Ok(ElevatedState::Done(completed)) => {
                let limits = self.limits.clone();
                self.label = format!("Loading {}", completed.snapshot.display());
                self.state = DriveState::Loading(WorkerJob::load(completed.snapshot, limits));
                JobState::Running {
                    label: self.label.clone(),
                    progress: None,
                }
            }
            Err(message) => JobState::Failed(message),
        }
    }
}

#[cfg(test)]
mod tests;
