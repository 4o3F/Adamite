use crate::{PublishMode, ScanJobError, ScanOptions, Source};
use adamite_core::scan::ScanOutput;
use adamite_io::{
    driver::DriverProgress,
    status::{ScanStatus, ScanStatusReader},
};
use std::{
    ffi::OsString,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

static NEXT_SCAN: AtomicU64 = AtomicU64::new(0);

/// Owns snapshot and status paths for a child running the current executable's `scan` command.
///
/// Dropping the owner removes the status file and any automatically selected temporary snapshot. Explicit snapshot destinations are preserved.
#[derive(Debug)]
pub struct ElevatedScan {
    snapshot: PathBuf,
    status: PathBuf,
    mode: PublishMode,
    remove_snapshot: bool,
}

impl ElevatedScan {
    /// Checks whether Windows administrator access is needed and elevation is permitted.
    ///
    /// # Errors
    /// Returns an elevation error if querying the Windows process token fails.
    pub fn required(limits: &ScanOptions) -> Result<bool, ScanJobError> {
        if limits.no_elevate {
            return Ok(false);
        }
        #[cfg(windows)]
        {
            adamite_io::elevation::ElevatedProcess::is_current()
                .map(|elevated| !elevated)
                .map_err(ScanJobError::from)
        }
        #[cfg(not(windows))]
        {
            Ok(false)
        }
    }

    /// Selects unique status paths and an optional explicit snapshot destination.
    ///
    /// # Errors
    /// Returns path resolution or inspection errors, or exhausts the path collision retries.
    pub fn new(destination: Option<(&Path, PublishMode)>) -> Result<Self, ScanJobError> {
        let destination = destination
            .map(|(path, mode)| {
                std::path::absolute(path)
                    .map(|path| (path, mode))
                    .map_err(|source| ScanJobError::Io {
                        operation: "resolve snapshot destination",
                        path: path.to_owned(),
                        source,
                    })
            })
            .transpose()?;
        for _ in 0..16 {
            let sequence = NEXT_SCAN.fetch_add(1, Ordering::Relaxed);
            let (snapshot, status) = Self::paths(
                destination.as_ref().map(|(path, _)| path.as_path()),
                std::process::id(),
                sequence,
            );
            if Self::exists(&status)? || (destination.is_none() && Self::exists(&snapshot)?) {
                continue;
            }
            return Ok(Self {
                snapshot,
                status,
                mode: destination
                    .as_ref()
                    .map_or(PublishMode::NoClobber, |(_, mode)| *mode),
                remove_snapshot: destination.is_none(),
            });
        }
        Err(ScanJobError::Io {
            operation: "choose elevated scan paths",
            path: std::env::temp_dir(),
            source: io::Error::from(io::ErrorKind::AlreadyExists),
        })
    }

    /// Uses explicit snapshot and status paths for a caller-managed scan job.
    ///
    /// The status path must not exist. Explicit snapshot destinations survive owner drop.
    ///
    /// # Errors
    /// Returns path resolution, inspection or status collision errors.
    pub fn with_status(
        destination: &Path,
        mode: PublishMode,
        status: &Path,
    ) -> Result<Self, ScanJobError> {
        let absolute = |path: &Path| {
            std::path::absolute(path).map_err(|source| ScanJobError::Io {
                operation: "resolve scan path",
                path: path.to_owned(),
                source,
            })
        };
        let snapshot = absolute(destination)?;
        let status = absolute(status)?;
        if Self::exists(&status)? {
            return Err(ScanJobError::StatusExists { path: status });
        }
        Ok(Self {
            snapshot,
            status,
            mode,
            remove_snapshot: false,
        })
    }

    fn paths(destination: Option<&Path>, pid: u32, sequence: u64) -> (PathBuf, PathBuf) {
        let snapshot = destination.map_or_else(
            || std::env::temp_dir().join(format!("adamite-{pid}-{sequence}.adm")),
            Path::to_owned,
        );
        let mut name = OsString::from(".");
        name.push(snapshot.file_name().unwrap_or_default());
        name.push(format!(".{pid}-{sequence}.status.jsonl"));
        let status = snapshot.with_file_name(name);
        (snapshot, status)
    }

    fn exists(path: &Path) -> Result<bool, ScanJobError> {
        match fs::symlink_metadata(path) {
            Ok(_) => Ok(true),
            Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(source) => Err(ScanJobError::Io {
                operation: "inspect elevated scan path",
                path: path.to_owned(),
                source,
            }),
        }
    }

    fn child_arguments(&self, drive: &Path, limits: &ScanOptions) -> Vec<OsString> {
        let mut args = vec![
            "scan".into(),
            drive.as_os_str().to_owned(),
            "-o".into(),
            self.snapshot.as_os_str().to_owned(),
            "--status-file".into(),
            self.status.as_os_str().to_owned(),
            "--no-elevate".into(),
        ];
        args.extend(limits.child_arguments());
        if matches!(self.mode, PublishMode::Replace) {
            args.push("--force".into());
        }
        args
    }

    /// Launches with the destination and publication mode selected by [`Self::new`].
    ///
    /// # Errors
    /// Returns an elevation error if administrator access is declined or launching fails.
    pub fn launch(self, drive: &Path, limits: &ScanOptions) -> Result<ElevatedJob, ScanJobError> {
        let process = Self::launch_process(self.child_arguments(drive, limits))?;
        Ok(ElevatedJob::new(self, process))
    }

    /// Launches, monitors and waits for the child, then loads its saved snapshot.
    ///
    /// Prints the administrator-access request before launching. The observer factory runs only after launch succeeds.
    ///
    /// # Errors
    /// Returns elevation, status, child outcome or completed-snapshot loading errors.
    pub fn run<O: FnMut(DriverProgress)>(
        self,
        drive: &Path,
        limits: &ScanOptions,
        observer: impl FnOnce() -> O,
    ) -> Result<ScanOutput, ScanJobError> {
        let _ = writeln!(
            io::stderr().lock(),
            "requesting administrator access for {}…",
            drive.display()
        );
        let mut job = self.launch(drive, limits)?;
        let result = job.monitor(observer());
        let waited = job.process.wait();
        if result.is_ok() {
            waited?;
        }
        let completed = result?;
        Source::open(&completed.snapshot)?
            .load_observed(&limits.limits(), |_| {})
            .map_err(ScanJobError::from)
    }
}

impl ElevatedScan {
    fn launch_process(arguments: Vec<OsString>) -> Result<Box<dyn ChildProcess>, ScanJobError> {
        #[cfg(windows)]
        {
            let process = adamite_io::elevation::ElevatedProcess::launch(arguments)
                .map_err(ScanJobError::from)?;
            Ok(Box::new(process))
        }
        #[cfg(not(windows))]
        {
            let _ = arguments;
            Err(ScanJobError::from(
                adamite_io::elevation::ElevationError::Launch {
                    source: io::Error::from(io::ErrorKind::Unsupported),
                },
            ))
        }
    }
}

/// Process operations needed by a polling scan, independent of any terminal view.
trait ChildProcess: std::fmt::Debug {
    fn try_wait(&mut self) -> Result<Option<u32>, ScanJobError>;
    fn wait(&mut self) -> Result<u32, ScanJobError>;
}

#[cfg(windows)]
impl ChildProcess for adamite_io::elevation::ElevatedProcess {
    fn try_wait(&mut self) -> Result<Option<u32>, ScanJobError> {
        adamite_io::elevation::ElevatedProcess::try_wait(self).map_err(ScanJobError::from)
    }
    fn wait(&mut self) -> Result<u32, ScanJobError> {
        adamite_io::elevation::ElevatedProcess::wait(self).map_err(ScanJobError::from)
    }
}

/// The snapshot location and save confirmation reported by a completed child.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletedScan {
    /// Native path owned by the job; a temporary snapshot is removed when the job drops.
    pub snapshot: PathBuf,
    /// Destination text reported by the child.
    pub saved: String,
    /// Encoded snapshot length reported by the child.
    pub bytes: u64,
}

/// The latest progress or successful outcome of an elevated child.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobState {
    /// The child is running, with the latest progress from this poll if available.
    Running(Option<DriverProgress>),
    /// The child exited and reported a saved snapshot.
    Done(CompletedScan),
}

/// Owns the child and scan paths; polling neither sleeps nor renders a terminal view.
#[derive(Debug)]
pub struct ElevatedJob {
    // Close file and process handles before the path owner attempts cleanup.
    reader: Option<ScanStatusReader>,
    process: Box<dyn ChildProcess>,
    progress: Vec<DriverProgress>,
    outcome: Option<ScanStatus>,
    exit_code: Option<u32>,
    scan: ElevatedScan,
}

impl ElevatedJob {
    fn new(scan: ElevatedScan, process: Box<dyn ChildProcess>) -> Self {
        Self {
            reader: None,
            process,
            progress: Vec::new(),
            outcome: None,
            exit_code: None,
            scan,
        }
    }

    /// Reads available status lines and checks process exit without sleeping.
    ///
    /// After exit, closes the status reader and returns the same terminal outcome on later polls.
    ///
    /// # Errors
    /// Returns status I/O or decoding errors, process errors, the child's reported failure or an exit without a final status line.
    pub fn poll(&mut self) -> Result<JobState, ScanJobError> {
        self.progress.clear();
        if let Some(exit_code) = self.exit_code {
            return self.completed(exit_code);
        }
        let exited = self.process.try_wait()?;
        if self.reader.is_none() {
            match ScanStatusReader::open(&self.scan.status) {
                Ok(reader) => self.reader = Some(reader),
                Err(source) if source.kind() == io::ErrorKind::NotFound => {}
                Err(source) => {
                    return Err(ScanJobError::Io {
                        operation: "open scan status",
                        path: self.scan.status.clone(),
                        source,
                    });
                }
            }
        }
        if let Some(reader) = &mut self.reader {
            for line in reader
                .poll()
                .map_err(|source| ScanJobError::Status { source })?
            {
                if let Some(progress) = line.driver_progress() {
                    self.progress.push(progress);
                } else if matches!(line, ScanStatus::Done { .. } | ScanStatus::Failed { .. }) {
                    self.outcome = Some(line);
                }
            }
        }
        if let Some(exit_code) = exited {
            self.exit_code = Some(exit_code);
            self.reader = None;
            self.completed(exit_code)
        } else {
            Ok(JobState::Running(self.progress.last().copied()))
        }
    }

    fn completed(&self, exit_code: u32) -> Result<JobState, ScanJobError> {
        match &self.outcome {
            Some(ScanStatus::Done { saved, bytes }) => Ok(JobState::Done(CompletedScan {
                snapshot: self.scan.snapshot.clone(),
                saved: saved.clone(),
                bytes: *bytes,
            })),
            Some(ScanStatus::Failed { message, exit_code }) => Err(ScanJobError::ChildFailed {
                message: message.clone(),
                exit_code: *exit_code,
            }),
            _ => Err(ScanJobError::ChildExited { exit_code }),
        }
    }

    fn monitor(
        &mut self,
        mut observer: impl FnMut(DriverProgress),
    ) -> Result<CompletedScan, ScanJobError> {
        loop {
            let state = self.poll();
            for progress in &self.progress {
                observer(*progress);
            }
            match state? {
                JobState::Done(completed) => return Ok(completed),
                JobState::Running(_) => std::thread::sleep(Duration::from_millis(100)),
            }
        }
    }
}

impl Drop for ElevatedScan {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.status);
        if self.remove_snapshot {
            let _ = fs::remove_file(&self.snapshot);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ChildProcess, ElevatedJob, ElevatedScan, JobState};
    use crate::{PublishMode, ReadRate, ScanJobError, ScanOptions, StatusFile};
    use adamite_core::scan::{ScanPhase, ScanProgress};
    use adamite_io::limits::ScanProfile;
    use adamite_io::{
        driver::DriverProgress,
        status::{ScanStatus, ScanStatusReader, ScanStatusWriter},
    };
    use std::num::{NonZeroU64, NonZeroUsize};
    use std::{cell::Cell, error::Error, ffi::OsString, fmt, fs, path::Path, rc::Rc};

    struct TestProcess<F>(F);

    impl<F> fmt::Debug for TestProcess<F> {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("TestProcess")
        }
    }

    impl<F: FnMut() -> Result<Option<u32>, ScanJobError>> ChildProcess for TestProcess<F> {
        fn try_wait(&mut self) -> Result<Option<u32>, ScanJobError> {
            (self.0)()
        }

        fn wait(&mut self) -> Result<u32, ScanJobError> {
            self.try_wait()?
                .ok_or(ScanJobError::ChildExited { exit_code: 1 })
        }
    }

    fn job<F>(scan: ElevatedScan, poll: F) -> ElevatedJob
    where
        F: FnMut() -> Result<Option<u32>, ScanJobError> + 'static,
    {
        ElevatedJob::new(scan, Box::new(TestProcess(poll)))
    }

    #[test]
    fn dropping_a_live_job_does_not_poll_or_wait_for_the_child() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let snapshot = directory.path().join("C.adm");
        let status = directory.path().join(".C.status.jsonl");
        let scan = ElevatedScan::with_status(&snapshot, PublishMode::Replace, &status)?;
        fs::write(&snapshot, b"published snapshot")?;
        let calls = Rc::new(Cell::new(0));
        let child_calls = Rc::clone(&calls);
        let job = job(scan, move || {
            child_calls.set(child_calls.get() + 1);
            Ok(None)
        });
        drop(job);
        assert_eq!(calls.get(), 0);
        assert!(snapshot.exists());
        Ok(())
    }

    #[test]
    fn child_arguments_forward_only_explicit_scan_limits() -> Result<(), Box<dyn Error>> {
        let scan = ElevatedScan::new(None)?;
        let base = vec![
            OsString::from("scan"),
            "C:".into(),
            "-o".into(),
            scan.snapshot.as_os_str().to_owned(),
            "--status-file".into(),
            scan.status.as_os_str().to_owned(),
            "--no-elevate".into(),
        ];
        assert_eq!(
            scan.child_arguments(Path::new("C:"), &ScanOptions::default()),
            base
        );
        let options = ScanOptions {
            profile: Some(ScanProfile::Background),
            max_read_rate: Some(ReadRate::new(
                "128MiB".into(),
                NonZeroU64::new(128 << 20).ok_or("rate")?,
            )),
            threads: NonZeroUsize::new(7),
            ..ScanOptions::default()
        };
        let mut expected = base;
        expected.extend(
            [
                "--profile",
                "background",
                "--max-read-rate",
                "128MiB",
                "--threads",
                "7",
            ]
            .map(OsString::from),
        );
        assert_eq!(scan.child_arguments(Path::new("C:"), &options), expected);
        let options = ScanOptions {
            profile: Some(ScanProfile::Balanced),
            threads: NonZeroUsize::new(1),
            ..ScanOptions::default()
        };
        assert_eq!(
            options.child_arguments(),
            ["--profile", "balanced", "--threads", "1"].map(OsString::from)
        );
        Ok(())
    }

    #[test]
    fn force_is_forwarded_only_for_replacement_destinations() -> Result<(), Box<dyn Error>> {
        let temporary = ElevatedScan::new(None)?;
        for (mode, force) in [
            (PublishMode::NoClobber, false),
            (PublishMode::Replace, true),
        ] {
            let scan = ElevatedScan::new(Some((&temporary.snapshot, mode)))?;
            let args = scan.child_arguments(Path::new("C:"), &ScanOptions::default());
            assert_eq!(args.iter().any(|arg| arg == "--force"), force);
            assert!(!scan.remove_snapshot);
        }
        assert!(temporary.remove_snapshot);
        Ok(())
    }

    #[test]
    fn snapshot_and_status_names_preserve_destination_directory() -> Result<(), Box<dyn Error>> {
        let destination = Path::new("folder with spaces/volume.adm");
        let (snapshot, status) = ElevatedScan::paths(Some(destination), 123, 7);
        assert_eq!(snapshot, destination);
        assert_eq!(
            status,
            Path::new("folder with spaces/.volume.adm.123-7.status.jsonl")
        );
        let (snapshot, status) = ElevatedScan::paths(None, 123, 8);
        assert_eq!(snapshot, std::env::temp_dir().join("adamite-123-8.adm"));
        assert_eq!(
            status,
            std::env::temp_dir().join(".adamite-123-8.adm.123-8.status.jsonl")
        );
        let first = ElevatedScan::new(None)?;
        let second = ElevatedScan::new(None)?;
        assert_ne!(first.snapshot, second.snapshot);
        assert_ne!(first.status, second.status);
        assert!(!first.snapshot.exists());
        assert!(!first.status.exists());
        Ok(())
    }

    #[test]
    fn monitor_drains_final_status_after_exit_and_cleans_closed_files() -> Result<(), Box<dyn Error>>
    {
        for exit_code in [1, 2, 3, 4] {
            let scan = ElevatedScan::new(None)?;
            fs::write(&scan.snapshot, b"temporary snapshot")?;
            let mut writer = ScanStatusWriter::create(&scan.status)?;
            writer.write(&ScanStatus::Failed {
                message: "child message".into(),
                exit_code,
            })?;
            drop(writer);
            let process_code = u32::try_from(exit_code)?;
            let mut job = job(scan, move || Ok(Some(process_code)));
            let error = job.monitor(|_| {}).err().ok_or("child failure accepted")?;
            assert_eq!(error.to_string(), "child message");
            assert!(
                matches!(error, ScanJobError::ChildFailed { exit_code: code, .. } if code == exit_code)
            );
            let snapshot = job.scan.snapshot.clone();
            let status = job.scan.status.clone();
            drop(job);
            assert!(!snapshot.exists());
            assert!(!status.exists());
        }
        let scan = ElevatedScan::new(None)?;
        let mut job = job(scan, || Ok(Some(9)));
        let error = job
            .monitor(|_| {})
            .err()
            .ok_or("missing outcome accepted")?;
        assert!(matches!(error, ScanJobError::ChildExited { exit_code: 9 }));
        assert!(error.to_string().contains("code 9"));
        Ok(())
    }

    #[test]
    fn monitor_waits_for_status_and_process_and_relays_progress() -> Result<(), Box<dyn Error>> {
        let scan = ElevatedScan::new(None)?;
        let relay_paths = ElevatedScan::new(None)?;
        let mut relay = StatusFile::create(&relay_paths.status)?;
        let progress = DriverProgress {
            scan: ScanProgress {
                phase: ScanPhase::Stream,
                records_processed: 10,
                records_planned: Some(20),
                outstanding: 0,
                waiting_for_input: false,
            },
            bytes_read: 4096,
        };
        let counter = Rc::new(Cell::new(0u32));
        let polled = Rc::clone(&counter);
        let status_path = scan.status.clone();
        let snapshot_path = scan.snapshot.clone();
        let mut job = job(scan, move || {
            let polls = polled.get().saturating_add(1);
            polled.set(polls);
            if polls == 2 {
                let mut writer =
                    ScanStatusWriter::create(&status_path).map_err(|source| ScanJobError::Io {
                        operation: "create test status",
                        path: status_path.clone(),
                        source,
                    })?;
                for status in [
                    ScanStatus::from_progress(&progress),
                    ScanStatus::Done {
                        saved: snapshot_path.to_string_lossy().into_owned(),
                        bytes: 100,
                    },
                ] {
                    writer
                        .write(&status)
                        .map_err(|source| ScanJobError::Status { source })?;
                }
            }
            Ok((polls >= 3).then_some(0))
        });
        job.monitor(|progress| relay.progress(&progress))?;
        let polls = counter.get();
        assert_eq!(polls, 3);
        drop(relay);
        let mut reader = ScanStatusReader::open(&relay_paths.status)?;
        assert_eq!(reader.poll()?, [ScanStatus::from_progress(&progress)]);
        drop(reader);
        Ok(())
    }

    #[test]
    fn poll_returns_latest_progress_then_saved_outcome_without_a_view() -> Result<(), Box<dyn Error>>
    {
        let scan = ElevatedScan::new(None)?;
        let status_path = scan.status.clone();
        let snapshot_path = scan.snapshot.clone();
        let exited = Rc::new(Cell::new(false));
        let process_exited = Rc::clone(&exited);
        let mut job = job(scan, move || Ok(process_exited.get().then_some(0)));
        assert_eq!(job.poll()?, JobState::Running(None));
        let progress = |records_processed| DriverProgress {
            scan: ScanProgress {
                phase: ScanPhase::Stream,
                records_processed,
                records_planned: Some(20),
                outstanding: 0,
                waiting_for_input: false,
            },
            bytes_read: 4096,
        };
        let mut writer = ScanStatusWriter::create(&status_path)?;
        writer.write(&ScanStatus::from_progress(&progress(1)))?;
        writer.write(&ScanStatus::from_progress(&progress(2)))?;
        assert_eq!(job.poll()?, JobState::Running(Some(progress(2))));
        assert_eq!(job.progress, [progress(1), progress(2)]);
        assert_eq!(job.poll()?, JobState::Running(None));
        fs::write(&snapshot_path, b"saved snapshot")?;
        writer.write(&ScanStatus::Done {
            saved: "C.adm".into(),
            bytes: 123,
        })?;
        drop(writer);
        assert_eq!(job.poll()?, JobState::Running(None));
        exited.set(true);
        let completed = job.poll()?;
        let JobState::Done(outcome) = &completed else {
            return Err("missing completed outcome".into());
        };
        assert_eq!(outcome.snapshot, snapshot_path);
        assert_eq!(outcome.saved, "C.adm");
        assert_eq!(outcome.bytes, 123);
        assert_eq!(fs::read(&outcome.snapshot)?, b"saved snapshot");
        assert!(job.reader.is_none());
        assert_eq!(job.poll()?, completed);
        drop(job);
        assert!(!status_path.exists());
        assert!(!snapshot_path.exists());
        Ok(())
    }

    #[test]
    fn dropping_job_closes_reader_and_preserves_explicit_destination() -> Result<(), Box<dyn Error>>
    {
        let paths = ElevatedScan::new(None)?;
        fs::write(&paths.snapshot, b"saved snapshot")?;
        let scan = ElevatedScan::new(Some((&paths.snapshot, PublishMode::Replace)))?;
        let status_path = scan.status.clone();
        let writer = ScanStatusWriter::create(&status_path)?;
        drop(writer);
        let mut job = job(scan, || Ok(None));
        assert_eq!(job.poll()?, JobState::Running(None));
        assert!(job.reader.is_some());
        drop(job);
        assert!(!status_path.exists());
        assert_eq!(fs::read(&paths.snapshot)?, b"saved snapshot");
        Ok(())
    }

    #[test]
    fn monitor_relays_each_phase_from_one_poll_before_completion() -> Result<(), Box<dyn Error>> {
        let scan = ElevatedScan::new(None)?;
        let relay_paths = ElevatedScan::new(None)?;
        let mut relay = StatusFile::create(&relay_paths.status)?;
        let mut writer = ScanStatusWriter::create(&scan.status)?;
        let mut expected = Vec::new();
        for phase in [ScanPhase::Boot, ScanPhase::Stream, ScanPhase::Finalize] {
            let progress = DriverProgress {
                scan: ScanProgress {
                    phase,
                    records_processed: 10,
                    records_planned: Some(20),
                    outstanding: 0,
                    waiting_for_input: false,
                },
                bytes_read: 4096,
            };
            let status = ScanStatus::from_progress(&progress);
            writer.write(&status)?;
            expected.push(status);
        }
        writer.write(&ScanStatus::Done {
            saved: scan.snapshot.to_string_lossy().into_owned(),
            bytes: 100,
        })?;
        drop(writer);
        let mut job = job(scan, || Ok(Some(0)));
        job.monitor(|progress| relay.progress(&progress))?;
        drop(relay);
        let mut reader = ScanStatusReader::open(&relay_paths.status)?;
        assert_eq!(reader.poll()?, expected);
        drop(reader);
        Ok(())
    }

    #[test]
    fn malformed_status_error_retains_cleanup() -> Result<(), Box<dyn Error>> {
        let scan = ElevatedScan::new(None)?;
        let status_path = scan.status.clone();
        let snapshot_path = scan.snapshot.clone();
        fs::write(&snapshot_path, b"temporary snapshot")?;
        fs::write(&status_path, b"invalid json\n")?;
        let mut job = job(scan, || Ok(None));
        assert!(matches!(job.poll(), Err(ScanJobError::Status { .. })));
        drop(job);
        assert!(!status_path.exists());
        assert!(!snapshot_path.exists());
        Ok(())
    }

    #[test]
    fn decline_and_launch_failures_remain_distinct() {
        let declined = ScanJobError::from(adamite_io::elevation::ElevationError::Declined);
        assert_eq!(declined.to_string(), "elevation was declined");
        assert!(matches!(declined, ScanJobError::ElevationDeclined));
        let launch = ScanJobError::from(adamite_io::elevation::ElevationError::Launch {
            source: std::io::Error::other("system launch error"),
        });
        assert!(matches!(launch, ScanJobError::Elevation { .. }));
    }
}
