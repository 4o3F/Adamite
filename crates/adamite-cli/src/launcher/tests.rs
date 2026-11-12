use super::{CliSources, DriveJob, DriveState, ElevatedWorker, WorkerJob, progress};
use crate::{
    cli::{BrowseArgs, Cli, Command, LimitArgs},
    source::SourceExt,
};
use adamite_access::Source;
use adamite_access::{CompletedScan, JobState as ElevatedState, PublishMode, SnapshotFile};
use adamite_core::scan::{ScanOutput, ScanPhase, ScanProgress};
use adamite_fixtures::Fixture;
use adamite_io::{driver::DriverProgress, snapshot_dir::SnapshotDir};
use adamite_tui::{Entry, JobState, ScanJob, Sources};
use clap::Parser;
use std::{
    error::Error,
    fs,
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

struct Case {
    directory: tempfile::TempDir,
    image: PathBuf,
}

impl Case {
    fn new() -> TestResult<Self> {
        let directory = tempfile::tempdir()?;
        let fixture = Fixture::load(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/ntfs/basic-4k.json"),
        )?;
        let image = directory.path().join("source.img");
        fs::write(&image, fixture.image())?;
        Ok(Self { directory, image })
    }

    fn sources(&self) -> CliSources {
        CliSources::with_dir(
            LimitArgs::default(),
            Some(SnapshotDir::new(self.directory.path())),
        )
    }

    fn output(&self) -> TestResult<ScanOutput> {
        Ok(Source::open(&self.image)?.load_observed_cli(&LimitArgs::default(), |_| {})?)
    }

    fn snapshot(&self, name: &str) -> TestResult<PathBuf> {
        let path = self.directory.path().join(name);
        SnapshotFile::new(path.clone(), PublishMode::Replace).save(&self.output()?)?;
        Ok(path)
    }
}

fn finish(job: &mut dyn ScanJob) -> TestResult<ScanOutput> {
    let started = Instant::now();
    for _ in 0..1000 {
        match job.poll() {
            JobState::Done(output) => return Ok(output),
            JobState::Failed(error) => return Err(error.into()),
            JobState::Running { .. } => thread::sleep(Duration::from_millis(5)),
        }
        if started.elapsed() > Duration::from_secs(5) {
            break;
        }
    }
    Err("job did not finish within five seconds".into())
}

#[test]
fn browse_defaults_match_no_argument_launcher_limits_and_path_requires_source() -> TestResult {
    let cli = Cli::try_parse_from(["adamite", "browse"])?;
    let Some(Command::Browse(args)) = cli.command else {
        return Err("missing browse".into());
    };
    assert!(args.source.is_none());
    assert!(args.path.is_none());
    assert_eq!(args.limits.limits(), BrowseArgs::default().limits.limits());
    let cli = Cli::try_parse_from(["adamite", "browse", "C.adm", "/Users"])?;
    let Some(Command::Browse(args)) = cli.command else {
        return Err("missing browse".into());
    };
    assert_eq!(args.source, Some(PathBuf::from("C.adm")));
    assert_eq!(args.path.as_deref(), Some("/Users"));
    assert!(Cli::try_parse_from(["adamite", "browse", "", "/Users"]).is_err());
    Ok(())
}

#[test]
fn entries_include_snapshots_in_filename_order_and_skip_other_files() -> TestResult {
    let case = Case::new()?;
    case.snapshot("archive.AdM")?;
    case.snapshot("z-last.adm")?;
    fs::write(case.directory.path().join("ignore.txt"), b"ignored")?;
    fs::create_dir(case.directory.path().join("nested.adm"))?;
    let mut sources = case.sources();
    let entries = sources.entries();
    let names: Vec<_> = entries
        .iter()
        .filter_map(|entry| match entry {
            Entry::Snapshot(info) => Some(info.name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(names, ["archive", "z-last"]);
    for entry in entries {
        if let Entry::Snapshot(info) = entry {
            assert!(info.modified.is_some());
            assert!(info.error.is_none());
            assert!(Path::new(&info.path).is_file());
        }
    }
    Ok(())
}

#[cfg(windows)]
#[test]
fn drive_claims_only_its_single_letter_snapshot_ignoring_ascii_case() -> TestResult {
    let case = Case::new()?;
    let saved = case.snapshot("c.AdM")?;
    case.snapshot("CC.adm")?;
    let mut sources = case.sources();
    let entries = sources.entries();
    let snapshot = entries
        .iter()
        .find_map(|entry| match entry {
            Entry::Drive {
                letter: 'C',
                snapshot,
                ..
            } => snapshot.as_ref(),
            _ => None,
        })
        .ok_or("C did not claim c.AdM")?;
    assert_eq!(Path::new(&snapshot.path), saved);
    assert!(
        !entries
            .iter()
            .any(|entry| matches!(entry, Entry::Snapshot(info) if info.name == "c"))
    );
    assert!(
        entries
            .iter()
            .any(|entry| matches!(entry, Entry::Snapshot(info) if info.name == "CC"))
    );
    let output = finish(sources.open_path("c:").as_mut())?;
    assert_eq!(output.to_bytes()?, fs::read(&saved)?);
    Ok(())
}

#[cfg(unix)]
#[test]
fn unreadable_snapshot_is_an_error_row_and_other_rows_remain_available() -> TestResult {
    let case = Case::new()?;
    case.snapshot("good.adm")?;
    std::os::unix::fs::symlink(
        case.directory.path().join("missing"),
        case.directory.path().join("broken.adm"),
    )?;
    let mut sources = case.sources();
    let entries = sources.entries();
    let broken = entries
        .iter()
        .find(|entry| matches!(entry, Entry::Snapshot(info) if info.name == "broken"))
        .ok_or("broken snapshot not listed")?;
    assert!(
        matches!(broken, Entry::Snapshot(info) if info.error.is_some() && info.modified.is_none())
    );
    assert!(
        entries
            .iter()
            .any(|entry| matches!(entry, Entry::Snapshot(info) if info.name == "good"))
    );
    // Wait for the worker before removing the temporary directory on platforms that lock open files.
    let mut job = sources.start(broken, false);
    assert!(finish(job.as_mut()).is_err());
    Ok(())
}

#[test]
fn missing_directory_or_base_directory_leaves_snapshot_rows_empty() -> TestResult {
    let temporary = tempfile::tempdir()?;
    let missing = temporary.path().join("missing");
    let mut sources = CliSources::with_dir(LimitArgs::default(), Some(SnapshotDir::new(&missing)));
    assert!(
        !sources
            .entries()
            .iter()
            .any(|entry| matches!(entry, Entry::Snapshot(_)))
    );
    assert!(!missing.exists());
    let mut sources = CliSources::with_dir(LimitArgs::default(), None);
    assert!(
        !sources
            .entries()
            .iter()
            .any(|entry| matches!(entry, Entry::Snapshot(_)))
    );
    assert!(matches!(
        sources.open_path("D:").poll(),
        JobState::Failed(_)
    ));
    Ok(())
}

#[test]
fn open_image_runs_on_worker_and_does_not_save_a_snapshot() -> TestResult {
    let case = Case::new()?;
    let mut sources = case.sources();
    let mut job = sources.open_path(case.image.to_str().ok_or("image path")?);
    let output = finish(job.as_mut())?;
    assert_eq!(output.snapshot.len(), case.output()?.snapshot.len());
    assert!(
        !sources
            .entries()
            .iter()
            .any(|entry| matches!(entry, Entry::Snapshot(_)))
    );
    Ok(())
}

#[test]
fn missing_file_fails_and_snapshot_row_loads_on_worker() -> TestResult {
    let case = Case::new()?;
    let saved = case.snapshot("saved.adm")?;
    let mut sources = case.sources();
    let missing = case.directory.path().join("missing.img");
    assert!(
        matches!(sources.open_path(missing.to_str().ok_or("path")?).poll(), JobState::Failed(message) if message.contains("open source"))
    );
    let entry = sources
        .entries()
        .into_iter()
        .find(|entry| matches!(entry, Entry::Snapshot(info) if info.name == "saved"))
        .ok_or("saved snapshot")?;
    let output = finish(sources.start(&entry, false).as_mut())?;
    assert_eq!(output.to_bytes()?, fs::read(&saved)?);
    let output = finish(sources.open_path(saved.to_str().ok_or("path")?).as_mut())?;
    assert_eq!(output.to_bytes()?, fs::read(&saved)?);
    Ok(())
}

#[test]
fn worker_poll_coalesces_progress_and_uses_shared_phase_labels() -> TestResult {
    let (progress_sender, reports) = mpsc::channel();
    let (_result_sender, result) = mpsc::channel();
    let mut job = WorkerJob {
        label: "Scanning".into(),
        progress: reports,
        result,
    };
    let report = |records_processed| DriverProgress {
        scan: ScanProgress {
            phase: ScanPhase::Stream,
            records_processed,
            records_planned: Some(100),
            outstanding: 0,
            waiting_for_input: false,
        },
        bytes_read: 4096,
    };
    progress_sender.send(report(1))?;
    progress_sender.send(report(2))?;
    assert!(
        matches!(job.poll(), JobState::Running { progress: Some(latest), .. } if latest == progress(report(2)) && latest.phase == "reading MFT")
    );
    assert!(matches!(
        job.poll(),
        JobState::Running { progress: None, .. }
    ));
    drop(progress_sender);
    Ok(())
}

#[test]
fn completed_elevated_job_loads_snapshot_on_a_worker_and_preserves_it() -> TestResult {
    let case = Case::new()?;
    let saved = case.snapshot("C.adm")?;
    let (outgoing, responses) = mpsc::channel();
    let (requests, _incoming) = mpsc::sync_channel(1);
    outgoing.send(Ok(ElevatedState::Done(CompletedScan {
        snapshot: saved.clone(),
        saved: saved.to_string_lossy().into_owned(),
        bytes: fs::metadata(&saved)?.len(),
    })))?;
    let mut job = DriveJob {
        state: DriveState::Scanning(ElevatedWorker {
            requests,
            responses,
        }),
        limits: LimitArgs::default(),
        label: "Scanning C:".into(),
    };
    assert!(matches!(job.poll(), JobState::Running { label, .. } if label.starts_with("Loading")));
    let output = finish(&mut job)?;
    assert_eq!(output.to_bytes()?, fs::read(&saved)?);
    drop(job);
    assert!(saved.exists());
    Ok(())
}

#[test]
fn elevated_errors_keep_the_child_message() -> TestResult {
    let (outgoing, responses) = mpsc::channel();
    let (requests, _incoming) = mpsc::sync_channel(1);
    outgoing.send(Err("elevation was declined".to_owned()))?;
    let mut job = DriveJob {
        state: DriveState::Scanning(ElevatedWorker {
            requests,
            responses,
        }),
        limits: LimitArgs::default(),
        label: "Scanning C:".into(),
    };
    assert!(matches!(job.poll(), JobState::Failed(message) if message == "elevation was declined"));
    Ok(())
}
