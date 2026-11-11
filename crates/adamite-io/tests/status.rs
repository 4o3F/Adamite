//! Status wire format, incremental reads and deterministic progress throttling.

use adamite_core::scan::{ScanPhase, ScanProgress};
use adamite_io::{
    driver::DriverProgress,
    status::{ScanStatus, ScanStatusReader, ScanStatusWriter, StatusError},
};
use std::{
    error::Error,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

struct StatusFile(PathBuf);

impl StatusFile {
    fn new() -> io::Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let name = format!(
            "adamite-status-{}-{}-{}",
            std::env::consts::OS,
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let directory = std::env::temp_dir().join(name);
        fs::create_dir(&directory)?;
        Ok(Self(directory))
    }

    fn remove(self) -> io::Result<()> {
        fs::remove_file(self.path())?;
        fs::remove_dir(self.0)
    }

    fn path(&self) -> PathBuf {
        self.0.join("status.jsonl")
    }

    fn progress(phase: ScanPhase, records: u64) -> DriverProgress {
        DriverProgress {
            scan: ScanProgress {
                phase,
                records_processed: records,
                records_planned: Some(20),
                outstanding: 0,
                waiting_for_input: false,
            },
            bytes_read: 4096,
        }
    }
}

#[test]
fn variants_round_trip_with_exact_json_line_format() -> Result<(), Box<dyn Error>> {
    let file = StatusFile::new()?;
    let mut writer = ScanStatusWriter::create(file.path())?;
    let mut reader = ScanStatusReader::open(file.path())?;
    let cases = [
        (
            ScanStatus::Progress {
                phase: "stream".to_owned(),
                records: 10,
                planned: Some(20),
                bytes: 4096,
            },
            r#"{"progress":{"phase":"stream","records":10,"planned":20,"bytes":4096}}"#,
        ),
        (
            ScanStatus::Progress {
                phase: "boot".to_owned(),
                records: 0,
                planned: None,
                bytes: 0,
            },
            r#"{"progress":{"phase":"boot","records":0,"planned":null,"bytes":0}}"#,
        ),
        (
            ScanStatus::Done {
                saved: "数据.adm".to_owned(),
                bytes: 123,
            },
            r#"{"done":{"saved":"数据.adm","bytes":123}}"#,
        ),
        (
            ScanStatus::Failed {
                message: "scan\nfailed".to_owned(),
                exit_code: 4,
            },
            r#"{"failed":{"message":"scan\nfailed","exit_code":4}}"#,
        ),
    ];
    let mut expected = String::new();
    for (status, json) in cases {
        writer.write(&status)?;
        expected.push_str(json);
        expected.push('\n');
        assert_eq!(fs::read_to_string(file.path())?, expected);
        assert_eq!(reader.poll()?, [status]);
        assert!(reader.poll()?.is_empty());
    }
    drop(reader);
    drop(writer);
    file.remove()?;
    Ok(())
}

#[test]
fn partial_trailing_line_survives_multiple_polls() -> Result<(), Box<dyn Error>> {
    let file = StatusFile::new()?;
    let mut writer = OpenOptions::new()
        .append(true)
        .create_new(true)
        .open(file.path())?;
    let mut reader = ScanStatusReader::open(file.path())?;
    assert!(reader.poll()?.is_empty());
    let first = ScanStatus::from_progress(&StatusFile::progress(ScanPhase::Stream, 1));
    let second = ScanStatus::Done {
        saved: "数据.adm".to_owned(),
        bytes: 100,
    };
    let second_json = serde_json::to_vec(&second)?;
    let (start, end) = second_json.split_at(second_json.len() / 2);
    writer.write_all(&serde_json::to_vec(&first)?)?;
    writer.write_all(b"\n")?;
    writer.write_all(start)?;
    writer.flush()?;
    assert_eq!(reader.poll()?, [first]);
    assert!(reader.poll()?.is_empty());
    writer.write_all(end)?;
    writer.flush()?;
    assert!(reader.poll()?.is_empty());
    writer.write_all(b"\n")?;
    writer.flush()?;
    assert_eq!(reader.poll()?, [second]);
    assert!(reader.poll()?.is_empty());
    drop(reader);
    drop(writer);
    file.remove()?;
    Ok(())
}

#[test]
fn exclusive_creation_preserves_existing_contents() -> Result<(), Box<dyn Error>> {
    let file = StatusFile::new()?;
    fs::write(file.path(), b"existing contents")?;
    let error = ScanStatusWriter::create(file.path())
        .err()
        .ok_or("existing status file was accepted")?;
    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(fs::read(file.path())?, b"existing contents");
    file.remove()?;
    Ok(())
}

#[test]
fn malformed_complete_line_is_an_error_and_is_consumed() -> Result<(), Box<dyn Error>> {
    let file = StatusFile::new()?;
    fs::write(
        file.path(),
        b"{not json}\n{\"done\":{\"saved\":\"x.adm\",\"bytes\":1}}\n",
    )?;
    let mut reader = ScanStatusReader::open(file.path())?;
    let error = reader.poll().err().ok_or("invalid JSON was accepted")?;
    assert!(matches!(error, StatusError::Json { .. }));
    assert!(error.source().is_some());
    assert_eq!(
        reader.poll()?,
        [ScanStatus::Done {
            saved: "x.adm".to_owned(),
            bytes: 1,
        }]
    );
    drop(reader);
    file.remove()?;
    Ok(())
}

#[test]
fn progress_throttles_until_boundary_and_always_reports_phase_changes() -> Result<(), Box<dyn Error>>
{
    let file = StatusFile::new()?;
    let mut writer = ScanStatusWriter::create(file.path())?;
    let mut reader = ScanStatusReader::open(file.path())?;
    let now = Instant::now();
    let mut expected = Vec::new();
    for (milliseconds, phase, records, written) in [
        (0, ScanPhase::Stream, 1, true),
        (0, ScanPhase::Stream, 2, false),
        (499, ScanPhase::Stream, 3, false),
        (500, ScanPhase::Stream, 4, true),
        (500, ScanPhase::Merge, 5, true),
        (999, ScanPhase::Merge, 6, false),
        (1000, ScanPhase::Merge, 7, true),
        (500, ScanPhase::Merge, 8, false),
        (1000, ScanPhase::Link, 9, true),
    ] {
        let progress = StatusFile::progress(phase, records);
        writer.progress(&progress, now + Duration::from_millis(milliseconds))?;
        if written {
            expected.push(ScanStatus::from_progress(&progress));
        }
    }
    assert_eq!(reader.poll()?, expected);
    drop(reader);
    drop(writer);
    file.remove()?;
    Ok(())
}

#[test]
fn driver_progress_uses_stable_phase_names_and_preserves_counters() {
    for (phase, name) in [
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
    ] {
        assert_eq!(
            ScanStatus::from_progress(&StatusFile::progress(phase, 10)),
            ScanStatus::Progress {
                phase: name.to_owned(),
                records: 10,
                planned: Some(20),
                bytes: 4096,
            }
        );
    }
}

#[test]
fn progress_reports_round_trip_to_driver_counters() {
    for phase in [
        ScanPhase::Boot,
        ScanPhase::Record0,
        ScanPhase::Discover,
        ScanPhase::Stream,
        ScanPhase::Merge,
        ScanPhase::Link,
        ScanPhase::Snapshot,
        ScanPhase::Aggregate,
        ScanPhase::Finalize,
        ScanPhase::Done,
        ScanPhase::Failed,
    ] {
        let progress = StatusFile::progress(phase, 7);
        assert_eq!(
            ScanStatus::from_progress(&progress).driver_progress(),
            Some(progress)
        );
    }
    let unknown = ScanStatus::Progress {
        phase: "unknown".to_owned(),
        records: 1,
        planned: None,
        bytes: 0,
    };
    assert_eq!(unknown.driver_progress(), None);
    let done = ScanStatus::Done {
        saved: "x.adm".to_owned(),
        bytes: 1,
    };
    assert_eq!(done.driver_progress(), None);
}
