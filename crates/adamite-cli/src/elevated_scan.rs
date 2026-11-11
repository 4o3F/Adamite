use crate::{
    cli::LimitArgs, error::CliError, save::PublishMode, scan_progress_view::ScanProgressView,
    source::Source, status_file::StatusFile,
};
use adamite_core::scan::ScanOutput;
use adamite_io::status::{ScanStatus, ScanStatusReader};
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
        let arguments = self.child_arguments(drive, limits);
        #[cfg(windows)]
        let mut process = adamite_io::elevation::ElevatedProcess::launch(arguments)
            .map_err(CliError::elevation)?;
        #[cfg(not(windows))]
        let _ = arguments;
        let result = self.monitor(drive, status, || {
            #[cfg(windows)]
            {
                process.try_wait().map_err(CliError::elevation)
            }
            #[cfg(not(windows))]
            {
                Err(CliError::elevation(
                    adamite_io::elevation::ElevationError::Launch {
                        source: io::Error::from(io::ErrorKind::Unsupported),
                    },
                ))
            }
        });
        #[cfg(windows)]
        {
            let waited = process.wait().map_err(CliError::elevation);
            if result.is_ok() {
                waited?;
            }
        }
        result?;
        Source::open(&self.snapshot)?.load_in_process(limits, None)
    }

    fn monitor(
        &self,
        drive: &Path,
        mut status: Option<&mut StatusFile>,
        mut try_wait: impl FnMut() -> Result<Option<u32>, CliError>,
    ) -> Result<(), CliError> {
        let mut reader = None;
        let mut view = ScanProgressView::new(drive);
        let mut outcome = None;
        loop {
            let exited = try_wait()?;
            if reader.is_none() {
                match ScanStatusReader::open(&self.status) {
                    Ok(opened) => reader = Some(opened),
                    Err(source) if source.kind() == io::ErrorKind::NotFound => {}
                    Err(source) => {
                        return Err(CliError::Io {
                            operation: "open scan status",
                            path: self.status.clone(),
                            source,
                        });
                    }
                }
            }
            if let Some(reader) = &mut reader {
                for line in reader
                    .poll()
                    .map_err(|source| CliError::Status { source })?
                {
                    if let Some(progress) = line.driver_progress() {
                        if let Some(view) = &mut view {
                            view.update(progress);
                        }
                        if let Some(status) = &mut status {
                            status.progress(&progress);
                        }
                    } else if matches!(line, ScanStatus::Done { .. } | ScanStatus::Failed { .. }) {
                        outcome = Some(line);
                    }
                }
            }
            if let Some(exit_code) = exited {
                return match outcome {
                    Some(ScanStatus::Done { .. }) => Ok(()),
                    Some(ScanStatus::Failed { message, exit_code }) => {
                        Err(CliError::ChildFailed { message, exit_code })
                    }
                    _ => Err(CliError::ChildExited { exit_code }),
                };
            }
            std::thread::sleep(Duration::from_millis(100));
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
    use super::ElevatedScan;
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
    use std::{error::Error, ffi::OsString, fs, path::Path};

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
            let error = scan
                .monitor(Path::new("C:"), None, || Ok(Some(process_code)))
                .err()
                .ok_or("child failure accepted")?;
            assert_eq!(error.message(), "child message");
            assert_eq!(i32::from(error.exit_code()), exit_code);
            let snapshot = scan.snapshot.clone();
            let status = scan.status.clone();
            drop(scan);
            assert!(!snapshot.exists());
            assert!(!status.exists());
        }
        let scan = ElevatedScan::new(None)?;
        let error = scan
            .monitor(Path::new("C:"), None, || Ok(Some(9)))
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
        let mut polls: u32 = 0;
        scan.monitor(Path::new("C:"), Some(&mut relay), || {
            polls = polls.saturating_add(1);
            if polls == 2 {
                let mut writer =
                    ScanStatusWriter::create(&scan.status).map_err(|source| CliError::Io {
                        operation: "create test status",
                        path: scan.status.clone(),
                        source,
                    })?;
                for status in [
                    ScanStatus::from_progress(&progress),
                    ScanStatus::Done {
                        saved: scan.snapshot.to_string_lossy().into_owned(),
                        bytes: 100,
                    },
                ] {
                    writer
                        .write(&status)
                        .map_err(|source| CliError::Status { source })?;
                }
            }
            Ok((polls >= 3).then_some(0))
        })?;
        assert_eq!(polls, 3);
        drop(relay);
        let mut reader = ScanStatusReader::open(&relay_paths.status)?;
        assert_eq!(reader.poll()?, [ScanStatus::from_progress(&progress)]);
        drop(reader);
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
