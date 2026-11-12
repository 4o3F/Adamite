use super::SnapshotCatalog;
use crate::{JobStatus, ScanJob, ScanOptions};
use adamite_io::snapshot_dir::SnapshotDir;
use std::{error::Error, fs};

type TestResult = Result<(), Box<dyn Error>>;

#[test]
fn entries_include_snapshots_in_filename_order_and_skip_other_files() -> TestResult {
    let directory = tempfile::tempdir()?;
    fs::write(directory.path().join("archive.AdM"), b"snapshot")?;
    fs::write(directory.path().join("z-last.adm"), b"snapshot")?;
    fs::write(directory.path().join("ignore.txt"), b"ignored")?;
    fs::create_dir(directory.path().join("nested.adm"))?;
    let catalog = SnapshotCatalog::new(SnapshotDir::new(directory.path()));
    let entries = catalog.entries()?;
    let names: Vec<_> = entries
        .iter()
        .filter_map(|entry| entry.file_stem())
        .collect();
    assert_eq!(names, ["archive", "z-last"]);
    for entry in entries {
        assert!(entry.modified().is_some());
        assert!(entry.error().is_none());
        assert_eq!(entry.byte_len(), Some(8));
        assert!(entry.path().is_some_and(|path| path.is_file()));
    }
    fs::write(directory.path().join("fresh.adm"), b"new")?;
    assert_eq!(catalog.entries()?.len(), 3);
    Ok(())
}

#[test]
fn drive_claims_only_its_single_letter_snapshot_ignoring_ascii_case() -> TestResult {
    let directory = tempfile::tempdir()?;
    let saved = directory.path().join("c.AdM");
    fs::write(&saved, b"snapshot")?;
    fs::write(directory.path().join("CC.adm"), b"snapshot")?;
    fs::write(directory.path().join("d.adm"), b"snapshot")?;
    let catalog = SnapshotCatalog::new(SnapshotDir::new(directory.path()));
    let mut entries = catalog.entries()?;
    let claimed = SnapshotCatalog::claim_drive(&mut entries, 'C').ok_or("C did not claim c.AdM")?;
    assert_eq!(claimed.path(), Some(saved.as_path()));
    assert!(SnapshotCatalog::claim_drive(&mut entries, 'C').is_none());
    assert_eq!(entries.len(), 2);
    assert_eq!(
        catalog
            .for_drive('c')?
            .and_then(|entry| entry.path().map(ToOwned::to_owned)),
        Some(saved)
    );
    assert!(catalog.for_drive('1')?.is_none());
    assert!(catalog.for_drive('é')?.is_none());
    Ok(())
}

#[cfg(unix)]
#[test]
fn unreadable_snapshot_is_an_error_row_and_other_rows_remain_available() -> TestResult {
    let directory = tempfile::tempdir()?;
    fs::write(directory.path().join("good.adm"), b"snapshot")?;
    std::os::unix::fs::symlink(
        directory.path().join("missing"),
        directory.path().join("c.adm"),
    )?;
    let catalog = SnapshotCatalog::new(SnapshotDir::new(directory.path()));
    let entries = catalog.entries()?;
    assert_eq!(entries.len(), 2);
    let broken = catalog
        .for_drive('C')?
        .ok_or("broken snapshot not listed")?;
    assert!(broken.error().is_some());
    assert!(broken.modified().is_none());
    let mut job = ScanJob::open(
        broken.path().ok_or("missing broken path")?.to_owned(),
        ScanOptions::default(),
        Some(&catalog),
    );
    assert!(
        matches!(job.poll(), JobStatus::Failed(error) if error.message.contains("open source"))
    );
    Ok(())
}

#[test]
fn missing_directory_or_base_directory_leaves_snapshot_rows_empty() -> TestResult {
    let directory = tempfile::tempdir()?;
    let missing = directory.path().join("missing");
    let catalog = SnapshotCatalog::new(SnapshotDir::new(&missing));
    assert!(catalog.entries()?.is_empty());
    assert!(!missing.exists());
    let mut job = ScanJob::open("D:".into(), ScanOptions::default(), None);
    assert!(
        matches!(job.poll(), JobStatus::Failed(error) if error.message == "cannot locate snapshot directory")
    );
    Ok(())
}

#[test]
fn directory_errors_are_returned_and_duplicate_claims_follow_filename_order() -> TestResult {
    let directory = tempfile::tempdir()?;
    let file = directory.path().join("file");
    fs::write(&file, b"not a directory")?;
    assert!(
        SnapshotCatalog::new(SnapshotDir::new(file))
            .entries()
            .is_err()
    );
    fs::write(directory.path().join("C.adm"), b"first")?;
    fs::write(directory.path().join("c.adm"), b"second")?;
    let catalog = SnapshotCatalog::new(SnapshotDir::new(directory.path()));
    assert_eq!(
        catalog
            .for_drive('c')?
            .and_then(|entry| entry.path().map(ToOwned::to_owned)),
        Some(directory.path().join("C.adm"))
    );
    Ok(())
}
