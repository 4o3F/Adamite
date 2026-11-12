use super::{CliSources, JobAdapter};
use crate::cli::{BrowseArgs, Cli, Command, LimitArgs};
use adamite_access::{JobError, JobStatus, ScanJob as AccessJob, ScanOptions};
use adamite_core::scan::{ScanPhase, ScanProgress};
use adamite_io::{driver::DriverProgress, snapshot_dir::SnapshotDir};
use adamite_tui::{Entry, JobState, Sources};
use clap::Parser;
use std::{error::Error, fs, path::PathBuf};

type TestResult = Result<(), Box<dyn Error>>;

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
fn adapter_maps_progress_labels_and_child_messages() {
    let mut job = AccessJob::scan_drive('C', ScanOptions::default(), None);
    let report = DriverProgress {
        scan: ScanProgress {
            phase: ScanPhase::Stream,
            records_processed: 2,
            records_planned: Some(100),
            outstanding: 0,
            waiting_for_input: false,
        },
        bytes_read: 4096,
    };
    assert!(matches!(
        JobAdapter::state(&job, JobStatus::Running(Some(report))),
        JobState::Running { label, progress: Some(latest) }
            if label == "Scanning C:" && latest.phase == "reading MFT"
            && latest.records == 2 && latest.planned == Some(100) && latest.bytes == 4096
    ));
    assert!(matches!(
        JobAdapter::state(&job, JobStatus::Running(None)),
        JobState::Running { progress: None, .. }
    ));
    assert!(matches!(
        JobAdapter::state(&job, JobStatus::Failed(JobError {
            message: "child failed\ncaused by: denied".into(), exit_code: Some(4),
        })),
        JobState::Failed(message) if message == "child failed\ncaused by: denied"
    ));
    let status = job.poll();
    assert!(
        matches!(JobAdapter::state(&job, status), JobState::Failed(message) if message == "cannot locate snapshot directory")
    );
}

#[test]
fn adapter_preserves_snapshot_metadata_and_native_paths() -> TestResult {
    let temporary = tempfile::tempdir()?;
    let saved = temporary.path().join("saved.AdM");
    fs::write(&saved, b"snapshot")?;
    let mut sources = CliSources::with_dir(
        LimitArgs::default(),
        Some(SnapshotDir::new(temporary.path())),
    );
    let info = sources
        .entries()
        .into_iter()
        .find_map(|entry| match entry {
            Entry::Snapshot(info) if info.name == "saved" => Some(info),
            _ => None,
        })
        .ok_or("missing saved snapshot")?;
    assert_eq!(sources.snapshot_path(&info), saved);
    assert!(info.modified.is_some());
    assert!(info.error.is_none());
    Ok(())
}

#[test]
fn adapter_loads_snapshot_with_loading_label_and_transfers_output() -> TestResult {
    use adamite_access::{PublishMode, SnapshotFile, Source};
    use adamite_fixtures::Fixture;
    use std::{
        path::Path,
        thread,
        time::{Duration, Instant},
    };

    let directory = tempfile::tempdir()?;
    let fixture = Fixture::load(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/ntfs/basic-4k.json"),
    )?;
    let image = directory.path().join("source.img");
    fs::write(&image, fixture.image())?;
    let output = Source::open(&image)?.load_observed(&ScanOptions::default().limits(), |_| {})?;
    let saved = directory.path().join("saved.adm");
    SnapshotFile::new(saved.clone(), PublishMode::Replace).save(&output)?;
    let mut sources = CliSources::with_dir(
        LimitArgs::default(),
        Some(SnapshotDir::new(directory.path())),
    );
    let entry = sources
        .entries()
        .into_iter()
        .find(|entry| matches!(entry, Entry::Snapshot(info) if info.name == "saved"))
        .ok_or("missing saved snapshot")?;
    let mut adapter = JobAdapter(AccessJob::open(saved.clone(), ScanOptions::default(), None));
    assert!(
        matches!(JobAdapter::state(&adapter.0, JobStatus::Running(None)), JobState::Running { label, .. } if label == format!("Loading {}", saved.display()))
    );
    // Exercise the Sources adapter as well as the direct status mapping.
    let mut row_job = sources.start(&entry, false);
    let started = Instant::now();
    let mut completed = 0;
    while completed < 2 && started.elapsed() <= Duration::from_secs(5) {
        let state = if completed == 0 {
            adamite_tui::ScanJob::poll(&mut adapter)
        } else {
            row_job.poll()
        };
        match state {
            JobState::Done(actual) => {
                assert_eq!(actual, output);
                completed += 1;
            }
            JobState::Failed(message) => return Err(message.into()),
            JobState::Running { .. } => thread::sleep(Duration::from_millis(5)),
        }
    }
    assert_eq!(
        completed, 2,
        "both adapter jobs must finish before directory cleanup"
    );
    Ok(())
}
