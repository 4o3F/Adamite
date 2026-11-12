use super::{ElevatedWorker, ScanJob, State, WorkerJob};
use crate::{
    CompletedScan, JobError, JobState as ElevatedState, JobStatus, PublishMode, ScanJobError,
    ScanOptions, SnapshotFile, Source,
};
use adamite_core::scan::{ScanOutput, ScanPhase, ScanProgress};
use adamite_fixtures::Fixture;
use adamite_io::driver::DriverProgress;
use std::{
    error::Error,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
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

    fn output(&self) -> TestResult<ScanOutput> {
        Ok(Source::open(&self.image)?.load_observed(&ScanOptions::default().limits(), |_| {})?)
    }

    fn snapshot(&self, name: &str) -> TestResult<PathBuf> {
        let path = self.directory.path().join(name);
        SnapshotFile::new(path.clone(), PublishMode::Replace).save(&self.output()?)?;
        Ok(path)
    }
}

impl Case {
    fn finish(job: &mut ScanJob) -> TestResult<(Arc<ScanOutput>, Option<PathBuf>)> {
        let started = Instant::now();
        while started.elapsed() <= Duration::from_secs(5) {
            match job.poll() {
                JobStatus::Done(output, path) => return Ok((output, path)),
                JobStatus::Failed(error) => return Err(error.into()),
                JobStatus::Running(_) => thread::sleep(Duration::from_millis(5)),
            }
        }
        Err("job did not finish within five seconds".into())
    }
}

#[test]
fn worker_poll_coalesces_progress() -> TestResult {
    let (progress_sender, reports) = mpsc::channel();
    let (_result_sender, result) = mpsc::channel();
    let mut job = WorkerJob {
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
    assert!(matches!(job.poll(), JobStatus::Running(Some(latest)) if latest == report(2)));
    assert!(matches!(job.poll(), JobStatus::Running(None)));
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
    let mut job = ScanJob {
        state: State::Elevated(
            ElevatedWorker {
                requests,
                responses,
            },
            ScanOptions::default(),
        ),
        path: "C:".into(),
        loading: false,
    };
    assert!(matches!(job.poll(), JobStatus::Running(None)));
    assert!(job.is_loading());
    assert_eq!(job.path(), saved);
    let (output, published) = Case::finish(&mut job)?;
    assert_eq!(published.as_deref(), Some(saved.as_path()));
    assert_eq!(output.to_bytes()?, fs::read(&saved)?);
    drop(job);
    assert!(saved.exists());
    Ok(())
}

#[test]
fn open_image_and_snapshot_on_workers_without_publishing() -> TestResult {
    let case = Case::new()?;
    let mut image = ScanJob::open(case.image.clone(), ScanOptions::default(), None);
    assert!(!image.is_loading());
    assert_eq!(image.path(), case.image);
    let (output, published) = Case::finish(&mut image)?;
    let expected = case.output()?;
    assert_eq!(output.snapshot.len(), expected.snapshot.len());
    assert_eq!(output.stats, expected.stats);
    assert!(published.is_none());
    let saved = case.snapshot("saved.adm")?;
    let mut snapshot = ScanJob::open(saved.clone(), ScanOptions::default(), None);
    assert!(snapshot.is_loading());
    let (output, published) = Case::finish(&mut snapshot)?;
    assert_eq!(output.to_bytes()?, fs::read(&saved)?);
    assert!(published.is_none());
    Ok(())
}

#[test]
fn in_process_scan_publishes_and_replaces_the_snapshot() -> TestResult {
    let case = Case::new()?;
    let saved = case.directory.path().join("C.adm");
    fs::write(&saved, b"old snapshot")?;
    let file = SnapshotFile::new(saved.clone(), PublishMode::Replace);
    let source = Source::open(&case.image)?;
    file.preflight(&source)?;
    let mut job = ScanJob {
        state: ScanJob::load(source, ScanOptions::default(), Some((file, saved.clone()))),
        path: case.image.clone(),
        loading: false,
    };
    let (output, published) = Case::finish(&mut job)?;
    assert_eq!(published.as_deref(), Some(saved.as_path()));
    assert_eq!(output.to_bytes()?, fs::read(&saved)?);
    Ok(())
}

#[test]
fn failures_keep_source_messages_and_child_exit_codes() -> TestResult {
    let case = Case::new()?;
    let missing = case.directory.path().join("missing.img");
    let mut job = ScanJob::open(missing, ScanOptions::default(), None);
    assert!(
        matches!(job.poll(), JobStatus::Failed(error) if error.message.contains("cannot open source") && error.exit_code.is_none())
    );
    for exit_code in [-1, 4, 256] {
        let (outgoing, responses) = mpsc::channel();
        let (requests, _incoming) = mpsc::sync_channel(1);
        outgoing.send(Err(ScanJobError::ChildFailed {
            message: "elevation was declined\ncaused by: child cause".into(),
            exit_code,
        }
        .into()))?;
        let mut job = ScanJob {
            state: State::Elevated(
                ElevatedWorker {
                    requests,
                    responses,
                },
                ScanOptions::default(),
            ),
            path: "C:".into(),
            loading: false,
        };
        assert!(
            matches!(job.poll(), JobStatus::Failed(error) if error.message == "elevation was declined\ncaused by: child cause" && error.exit_code == Some(i64::from(exit_code)))
        );
    }
    let error = JobError::from(ScanJobError::ChildExited {
        exit_code: u32::MAX,
    });
    assert_eq!(error.exit_code, Some(i64::from(u32::MAX)));
    assert!(error.message.contains(&u32::MAX.to_string()));
    Ok(())
}

#[test]
fn jobs_can_move_to_another_thread_for_polling() -> TestResult {
    let case = Case::new()?;
    let job = ScanJob::open(case.image.clone(), ScanOptions::default(), None);
    let (sender, receiver) = mpsc::channel();
    let worker = thread::spawn(move || {
        let mut job = job;
        let _ = sender.send(Case::finish(&mut job).map_err(|error| error.to_string()));
    });
    let result = receiver.recv_timeout(Duration::from_secs(10));
    worker.join().map_err(|_| "poll worker panicked")?;
    let (output, published) = result??;
    let expected = case.output()?;
    assert_eq!(output.snapshot.len(), expected.snapshot.len());
    assert_eq!(output.stats, expected.stats);
    assert!(published.is_none());
    Ok(())
}

#[test]
fn disconnected_workers_fail_and_empty_elevated_polls_request_work() -> TestResult {
    let (progress_sender, progress) = mpsc::channel();
    let (result_sender, result) = mpsc::channel();
    drop(progress_sender);
    drop(result_sender);
    let mut worker = WorkerJob { progress, result };
    assert!(
        matches!(worker.poll(), JobStatus::Failed(error) if error.message == "scan worker stopped without a result")
    );
    let (outgoing, responses) = mpsc::channel();
    let (requests, incoming) = mpsc::sync_channel(1);
    let mut elevated = ElevatedWorker {
        requests,
        responses,
    };
    assert!(matches!(elevated.poll()?, ElevatedState::Running(None)));
    assert!(matches!(elevated.poll()?, ElevatedState::Running(None)));
    incoming.try_recv()?;
    assert!(incoming.try_recv().is_err());
    outgoing.send(Ok(ElevatedState::Running(None)))?;
    assert!(matches!(elevated.poll()?, ElevatedState::Running(None)));
    incoming.try_recv()?;
    drop(outgoing);
    assert!(
        matches!(elevated.poll(), Err(error) if error.message == "elevated scan worker stopped without a result")
    );
    Ok(())
}

#[cfg(not(windows))]
#[test]
fn drive_input_requires_windows_even_when_a_saved_snapshot_exists() -> TestResult {
    use crate::SnapshotCatalog;
    use adamite_io::snapshot_dir::SnapshotDir;

    let case = Case::new()?;
    case.snapshot("c.adm")?;
    let catalog = SnapshotCatalog::new(SnapshotDir::new(case.directory.path()));
    let mut job = ScanJob::open("c:".into(), ScanOptions::default(), Some(&catalog));
    assert!(
        matches!(Case::finish(&mut job), Err(error) if error.to_string() == "raw volumes require Windows")
    );
    Ok(())
}

#[test]
fn invalid_drive_letter_fails_without_creating_a_directory() -> TestResult {
    use adamite_io::snapshot_dir::SnapshotDir;

    let directory = tempfile::tempdir()?;
    let missing = directory.path().join("missing");
    let directory = SnapshotDir::new(missing);
    let mut job = ScanJob::scan_drive('é', ScanOptions::default(), Some(&directory));
    assert!(
        matches!(job.poll(), JobStatus::Failed(error) if error.message == "drive letter must be an ASCII letter")
    );
    assert!(!directory.path().exists());
    Ok(())
}
