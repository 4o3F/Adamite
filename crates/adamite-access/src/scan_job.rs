use crate::{
    ElevatedScan, JobError, JobState as ElevatedState, PublishMode, ScanOptions, SnapshotCatalog,
    SnapshotFile, Source,
};
use adamite_core::scan::ScanOutput;
use adamite_io::{driver::DriverProgress, snapshot_dir::SnapshotDir};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        mpsc::{self, Receiver, SyncSender, TryRecvError},
    },
    thread,
};

/// The newest progress or terminal result of a background source operation.
#[derive(Debug)]
pub enum JobStatus {
    /// Work continues, with only the latest progress received during this poll.
    Running(Option<DriverProgress>),
    /// Transfers the completed output and, for drive scans, its published snapshot path.
    Done(Arc<ScanOutput>, Option<PathBuf>),
    /// Work failed with its human message and any child exit code.
    Failed(JobError),
}

/// A movable, nonblocking source load or drive scan, independent of a user interface.
///
/// Workers detach when dropped. An elevated child continues publishing if the caller exits.
/// A successful output is transferred once; callers stop polling after a terminal result.
#[derive(Debug)]
pub struct ScanJob {
    state: State,
    path: PathBuf,
    loading: bool,
}

#[derive(Debug)]
enum State {
    Working(WorkerJob),
    Elevated(ElevatedWorker, ScanOptions),
    Failed(JobError),
}

impl ScanJob {
    /// Opens a file on a worker, or reuses a listed Windows drive's saved snapshot before scanning.
    ///
    /// Source detection runs immediately. Images report progress without publishing a snapshot.
    /// Drive scans require a catalog directory; absent saved snapshots are scanned and published.
    #[must_use]
    pub fn open(path: PathBuf, options: ScanOptions, catalog: Option<&SnapshotCatalog>) -> Self {
        match Source::open(&path) {
            Ok(Source::Drive { letter, .. }) => {
                if Self::listed_drive(letter)
                    && let Some(entry) =
                        catalog.and_then(|catalog| catalog.for_drive(letter).ok().flatten())
                    && let Some(saved) = entry.path()
                {
                    return Self::open(saved.to_owned(), options, None);
                }
                Self::scan_drive(letter, options, catalog.map(SnapshotCatalog::directory))
            }
            Ok(source) => Self {
                loading: source.is_snapshot(),
                path,
                state: Self::load(source, options, None),
            },
            Err(error) => Self::failed(path, error.into()),
        }
    }

    /// Starts a drive scan, replacing `<LETTER>.adm` in the supplied snapshot directory.
    ///
    /// Creates the directory and checks publication before scanning. UAC and child polling run
    /// on one worker thread; after the child exits, another worker loads the published snapshot.
    /// An absent directory produces a failed job with `cannot locate snapshot directory`.
    #[must_use]
    pub fn scan_drive(letter: char, options: ScanOptions, directory: Option<&SnapshotDir>) -> Self {
        if !letter.is_ascii_alphabetic() {
            return Self::failed(
                PathBuf::from(format!("{letter}:")),
                JobError::new("drive letter must be an ASCII letter"),
            );
        }
        let letter = letter.to_ascii_uppercase();
        let path = PathBuf::from(format!("{letter}:"));
        let state = Self::drive_state(&path, letter, options, directory);
        Self {
            state: state.unwrap_or_else(State::Failed),
            path,
            loading: false,
        }
    }

    /// Borrows the current scan source or the snapshot being loaded after an elevated scan.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reports whether the current operation loads a snapshot rather than scans a source.
    #[must_use]
    pub fn is_loading(&self) -> bool {
        self.loading
    }

    /// Drains available progress, keeping the latest report, and checks for completion without waiting.
    pub fn poll(&mut self) -> JobStatus {
        let (worker, options) = match &mut self.state {
            State::Working(worker) => return worker.poll(),
            State::Failed(error) => return JobStatus::Failed(error.clone()),
            State::Elevated(worker, options) => (worker, options),
        };
        match worker.poll() {
            Ok(ElevatedState::Running(latest)) => JobStatus::Running(latest),
            Ok(ElevatedState::Done(completed)) => {
                self.path = completed.snapshot;
                self.loading = true;
                let options = options.clone();
                let path = self.path.clone();
                self.state = WorkerJob::spawn(move |_| {
                    let output = Source::open(&path)?.load_observed(&options.limits(), |_| {})?;
                    Ok((output, Some(path)))
                });
                JobStatus::Running(None)
            }
            Err(error) => {
                self.state = State::Failed(error.clone());
                JobStatus::Failed(error)
            }
        }
    }

    fn listed_drive(letter: char) -> bool {
        #[cfg(windows)]
        {
            adamite_io::volume::Volume::list()
                .unwrap_or_default()
                .iter()
                .any(|volume| volume.letter().eq_ignore_ascii_case(&letter))
        }
        #[cfg(not(windows))]
        {
            let _ = letter;
            false
        }
    }

    fn failed(path: PathBuf, error: JobError) -> Self {
        Self {
            state: State::Failed(error),
            path,
            loading: false,
        }
    }

    fn load(source: Source, options: ScanOptions, file: Option<(SnapshotFile, PathBuf)>) -> State {
        WorkerJob::spawn(move |progress| {
            let output = source.load_observed(&options.limits(), |report| {
                let _ = progress.send(report);
            })?;
            let published = if let Some((file, path)) = file {
                file.save(&output)?;
                Some(path)
            } else {
                None
            };
            Ok((output, published))
        })
    }

    fn drive_state(
        path: &Path,
        letter: char,
        options: ScanOptions,
        directory: Option<&SnapshotDir>,
    ) -> Result<State, JobError> {
        let directory =
            directory.ok_or_else(|| JobError::new("cannot locate snapshot directory"))?;
        directory
            .create()
            .map_err(|error| JobError::new(error.to_string()))?;
        let source = Source::open(path)?;
        let destination = directory.drive_snapshot(letter);
        let file = SnapshotFile::new(destination.clone(), PublishMode::Replace);
        file.preflight(&source)?;
        if ElevatedScan::required(&options)? {
            let scan = ElevatedScan::new(Some((&destination, PublishMode::Replace)))?;
            Ok(State::Elevated(
                ElevatedWorker::spawn(scan, path.to_owned(), options.clone())?,
                options,
            ))
        } else {
            Ok(Self::load(source, options, Some((file, destination))))
        }
    }
}

type WorkerResult = Result<(ScanOutput, Option<PathBuf>), JobError>;

#[derive(Debug)]
struct WorkerJob {
    progress: Receiver<DriverProgress>,
    result: Receiver<WorkerResult>,
}

impl WorkerJob {
    fn spawn(
        work: impl FnOnce(mpsc::Sender<DriverProgress>) -> WorkerResult + Send + 'static,
    ) -> State {
        let (progress_sender, progress) = mpsc::channel();
        let (result_sender, result) = mpsc::channel();
        match thread::Builder::new()
            .name("adamite-scan".to_owned())
            .spawn(move || {
                let output = work(progress_sender);
                let _ = result_sender.send(output);
            }) {
            Ok(worker) => {
                drop(worker);
                State::Working(Self { progress, result })
            }
            Err(error) => {
                State::Failed(JobError::new(format!("cannot start scan worker: {error}")))
            }
        }
    }

    fn poll(&mut self) -> JobStatus {
        let latest = self.progress.try_iter().last();
        match self.result.try_recv() {
            Ok(Ok((output, published))) => JobStatus::Done(Arc::new(output), published),
            Ok(Err(error)) => JobStatus::Failed(error),
            Err(TryRecvError::Disconnected) => {
                JobStatus::Failed(JobError::new("scan worker stopped without a result"))
            }
            Err(TryRecvError::Empty) => JobStatus::Running(latest),
        }
    }
}

/// Keeps the UAC prompt and its process handle on one worker; polls request one response at a time.
#[derive(Debug)]
struct ElevatedWorker {
    requests: SyncSender<()>,
    responses: Receiver<Result<ElevatedState, JobError>>,
}

impl ElevatedWorker {
    fn spawn(scan: ElevatedScan, path: PathBuf, options: ScanOptions) -> Result<Self, JobError> {
        let (requests, incoming) = mpsc::sync_channel(1);
        let (outgoing, responses) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("adamite-elevation".to_owned())
            .spawn(move || {
                let mut job = match scan.launch(&path, &options) {
                    Ok(job) => job,
                    Err(error) => {
                        let _ = outgoing.send(Err(error.into()));
                        return;
                    }
                };
                while incoming.recv().is_ok() {
                    let state = job.poll().map_err(JobError::from);
                    let terminal = !matches!(state, Ok(ElevatedState::Running(_)));
                    if outgoing.send(state).is_err() || terminal {
                        break;
                    }
                }
            })
            .map_err(|error| JobError::new(format!("cannot start elevation worker: {error}")))?;
        drop(worker);
        Ok(Self {
            requests,
            responses,
        })
    }

    fn poll(&mut self) -> Result<ElevatedState, JobError> {
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
            Err(TryRecvError::Disconnected) => Err(JobError::new(
                "elevated scan worker stopped without a result",
            )),
        }
    }
}

#[cfg(test)]
mod tests;
