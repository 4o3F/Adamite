use crate::{
    cli::LimitArgs, error::CliError, save::PublishMode, scan_progress_view::ScanProgressView,
    source::Source, status_file::StatusFile,
};
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

#[derive(Debug)]
pub(crate) struct ElevatedScan {
    snapshot: PathBuf,
    status: PathBuf,
    mode: PublishMode,
    remove_snapshot: bool,
}

impl ElevatedScan {
    pub(crate) fn required(limits: &LimitArgs) -> Result<bool, CliError> {
        if limits.no_elevate {
            return Ok(false);
        }
        #[cfg(windows)]
        {
            adamite_io::elevation::ElevatedProcess::is_current()
                .map(|elevated| !elevated)
                .map_err(CliError::elevation)
        }
        #[cfg(not(windows))]
        {
            Ok(false)
        }
    }

    pub(crate) fn new(destination: Option<(&Path, PublishMode)>) -> Result<Self, CliError> {
        let destination = destination
            .map(|(path, mode)| {
                std::path::absolute(path)
                    .map(|path| (path, mode))
                    .map_err(|source| CliError::Io {
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
        Err(CliError::Io {
            operation: "choose elevated scan paths",
            path: std::env::temp_dir(),
            source: io::Error::from(io::ErrorKind::AlreadyExists),
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

    fn exists(path: &Path) -> Result<bool, CliError> {
        match fs::symlink_metadata(path) {
            Ok(_) => Ok(true),
            Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(source) => Err(CliError::Io {
                operation: "inspect elevated scan path",
                path: path.to_owned(),
                source,
            }),
        }
    }

    fn child_arguments(&self, drive: &Path, limits: &LimitArgs) -> Vec<OsString> {
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

    /// Launches with the explicit destination and publication mode selected by `new`.
    pub(crate) fn launch(self, drive: &Path, limits: &LimitArgs) -> Result<ElevatedJob, CliError> {
        let process = launch_process(self.child_arguments(drive, limits))?;
        Ok(ElevatedJob::new(self, process))
    }

    pub(crate) fn run(
        self,
        drive: &Path,
        limits: &LimitArgs,
        status: Option<&mut StatusFile>,
    ) -> Result<ScanOutput, CliError> {
        let _ = writeln!(
            io::stderr().lock(),
            "requesting administrator access for {}…",
            drive.display()
        );
        let mut job = self.launch(drive, limits)?;
        let result = job.monitor(drive, status);
        let waited = job.process.wait();
        if result.is_ok() {
            waited?;
        }
        let completed = result?;
        Source::open(&completed.snapshot)?.load_in_process(limits, None)
    }
}

/// Process operations needed by a polling scan, independent of any terminal view.
trait ChildProcess: std::fmt::Debug {
    fn try_wait(&mut self) -> Result<Option<u32>, CliError>;
    fn wait(&mut self) -> Result<u32, CliError>;
}

fn launch_process(arguments: Vec<OsString>) -> Result<Box<dyn ChildProcess>, CliError> {
    #[cfg(windows)]
    {
        let process = adamite_io::elevation::ElevatedProcess::launch(arguments)
            .map_err(CliError::elevation)?;
        Ok(Box::new(process))
    }
    #[cfg(not(windows))]
    {
        let _ = arguments;
        Err(CliError::elevation(
            adamite_io::elevation::ElevationError::Launch {
                source: io::Error::from(io::ErrorKind::Unsupported),
            },
        ))
    }
}

#[cfg(windows)]
impl ChildProcess for adamite_io::elevation::ElevatedProcess {
    fn try_wait(&mut self) -> Result<Option<u32>, CliError> {
        adamite_io::elevation::ElevatedProcess::try_wait(self).map_err(CliError::elevation)
    }
    fn wait(&mut self) -> Result<u32, CliError> {
        adamite_io::elevation::ElevatedProcess::wait(self).map_err(CliError::elevation)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CompletedScan {
    pub(crate) snapshot: PathBuf,
    pub(crate) saved: String,
    pub(crate) bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum JobState {
    Running(Option<DriverProgress>),
    Done(CompletedScan),
}

/// Owns the child and scan paths; polling neither sleeps nor renders a terminal view.
#[derive(Debug)]
pub(crate) struct ElevatedJob {
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

    pub(crate) fn poll(&mut self) -> Result<JobState, CliError> {
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
                    return Err(CliError::Io {
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
                .map_err(|source| CliError::Status { source })?
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

    fn completed(&self, exit_code: u32) -> Result<JobState, CliError> {
        match &self.outcome {
            Some(ScanStatus::Done { saved, bytes }) => Ok(JobState::Done(CompletedScan {
                snapshot: self.scan.snapshot.clone(),
                saved: saved.clone(),
                bytes: *bytes,
            })),
            Some(ScanStatus::Failed { message, exit_code }) => Err(CliError::ChildFailed {
                message: message.clone(),
                exit_code: *exit_code,
            }),
            _ => Err(CliError::ChildExited { exit_code }),
        }
    }

    fn monitor(
        &mut self,
        drive: &Path,
        mut status: Option<&mut StatusFile>,
    ) -> Result<CompletedScan, CliError> {
        let mut view = ScanProgressView::new(drive);
        loop {
            let state = self.poll();
            for progress in &self.progress {
                if let Some(view) = &mut view {
                    view.update(*progress);
                }
                if let Some(status) = &mut status {
                    status.progress(progress);
                }
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
    use crate::{
        cli::{Cli, Command, LimitArgs},
        error::CliError,
        save::PublishMode,
        status_file::StatusFile,
    };
    use adamite_core::scan::{ScanPhase, ScanProgress};
    use adamite_io::{
        driver::DriverProgress,
        status::{ScanStatus, ScanStatusReader, ScanStatusWriter},
    };
    use clap::Parser;
    use std::{cell::Cell, error::Error, ffi::OsString, fmt, fs, path::Path, rc::Rc};

    struct TestProcess<F>(F);

    impl<F> fmt::Debug for TestProcess<F> {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("TestProcess")
        }
    }

    impl<F: FnMut() -> Result<Option<u32>, CliError>> ChildProcess for TestProcess<F> {
        fn try_wait(&mut self) -> Result<Option<u32>, CliError> {
            (self.0)()
        }

        fn wait(&mut self) -> Result<u32, CliError> {
            self.try_wait()?
                .ok_or(CliError::ChildExited { exit_code: 1 })
        }
    }

    fn job<F>(scan: ElevatedScan, poll: F) -> ElevatedJob
    where
        F: FnMut() -> Result<Option<u32>, CliError> + 'static,
    {
        ElevatedJob::new(scan, Box::new(TestProcess(poll)))
    }

    fn limits(flags: &[&str]) -> Result<LimitArgs, Box<dyn Error>> {
        let cli = Cli::try_parse_from(
            ["adamite", "scan", "C:"]
                .into_iter()
                .chain(flags.iter().copied()),
        )?;
        let Some(Command::Scan(args)) = cli.command else {
            return Err("missing scan".into());
        };
        Ok(args.source.limits)
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
        assert_eq!(scan.child_arguments(Path::new("C:"), &limits(&[])?), base);
        let options = limits(&[
            "--profile",
            "background",
            "--max-read-rate",
            "128MiB",
            "--threads",
            "7",
            "--trace-chrome",
            "parent.json",
        ])?;
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
        let options = limits(&["--profile", "balanced", "--threads", "1"])?;
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
            let args = scan.child_arguments(Path::new("C:"), &limits(&[])?);
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
            let error = job
                .monitor(Path::new("C:"), None)
                .err()
                .ok_or("child failure accepted")?;
            assert_eq!(error.message(), "child message");
            assert_eq!(i32::from(error.exit_code()), exit_code);
            let snapshot = job.scan.snapshot.clone();
            let status = job.scan.status.clone();
            drop(job);
            assert!(!snapshot.exists());
            assert!(!status.exists());
        }
        let scan = ElevatedScan::new(None)?;
        let mut job = job(scan, || Ok(Some(9)));
        let error = job
            .monitor(Path::new("C:"), None)
            .err()
            .ok_or("missing outcome accepted")?;
        assert_eq!(error.exit_code(), 1);
        assert!(error.message().contains("code 9"));
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
                    ScanStatusWriter::create(&status_path).map_err(|source| CliError::Io {
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
                        .map_err(|source| CliError::Status { source })?;
                }
            }
            Ok((polls >= 3).then_some(0))
        });
        job.monitor(Path::new("C:"), Some(&mut relay))?;
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
        job.monitor(Path::new("C:"), Some(&mut relay))?;
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
        assert!(matches!(job.poll(), Err(CliError::Status { .. })));
        drop(job);
        assert!(!status_path.exists());
        assert!(!snapshot_path.exists());
        Ok(())
    }

    #[test]
    fn decline_and_launch_failures_use_permission_exit_code() {
        let declined = CliError::elevation(adamite_io::elevation::ElevationError::Declined);
        assert_eq!(declined.message(), "elevation was declined");
        assert_eq!(declined.exit_code(), 4);
        let launch = CliError::elevation(adamite_io::elevation::ElevationError::Launch {
            source: std::io::Error::other("system launch error"),
        });
        assert_eq!(launch.exit_code(), 4);
    }
}
