//! Snapshot directory discovery, metadata and per-entry failures.

use adamite_io::snapshot_dir::{SnapshotDir, SnapshotDirError};
use etcetera::BaseStrategy;
use std::{error::Error, ffi::OsStr, fs};

#[test]
fn default_path_uses_platform_data_directory() -> Result<(), Box<dyn Error>> {
    assert_eq!(
        SnapshotDir::default_path()?,
        etcetera::choose_base_strategy()?
            .data_dir()
            .join("adamite")
            .join("snapshots")
    );
    Ok(())
}

#[test]
fn missing_directory_is_empty_and_created_only_on_request() -> Result<(), Box<dyn Error>> {
    let temporary = tempfile::tempdir()?;
    let path = temporary.path().join("nested").join("snapshots");
    let directory = SnapshotDir::new(&path);
    assert_eq!(directory.path(), path);
    assert_eq!(directory.drive_snapshot('c'), path.join("C.adm"));
    assert_eq!(directory.drive_snapshot('Z'), path.join("Z.adm"));
    assert!(directory.entries()?.is_empty());
    assert!(!path.exists());
    directory.create()?;
    directory.create()?;
    assert!(path.is_dir());
    assert!(directory.entries()?.is_empty());
    Ok(())
}

#[test]
fn entries_filter_sort_and_report_file_metadata() -> Result<(), Box<dyn Error>> {
    let temporary = tempfile::tempdir()?;
    let directory = SnapshotDir::new(temporary.path());
    for (name, bytes) in [("z.adm", b"abc".as_slice()), ("A.AdM", b"12345")] {
        fs::write(temporary.path().join(name), bytes)?;
    }
    fs::write(temporary.path().join("ignored.txt"), b"not a snapshot")?;
    fs::create_dir(temporary.path().join("directory.adm"))?;
    fs::write(
        temporary.path().join("directory.adm").join("nested.adm"),
        b"nested snapshot",
    )?;
    let entries = directory.entries()?;
    let mut entries = entries.iter();
    for (name, stem, size) in [("A.AdM", "A", 5), ("z.adm", "z", 3)] {
        let entry = entries.next().ok_or("missing snapshot")?;
        let path = temporary.path().join(name);
        assert_eq!(entry.path(), Some(path.as_path()));
        assert_eq!(entry.file_stem(), Some(OsStr::new(stem)));
        assert_eq!(entry.byte_len(), Some(size));
        assert_eq!(entry.modified(), Some(fs::metadata(&path)?.modified()?));
        assert!(entry.error().is_none());
    }
    assert!(entries.next().is_none());
    Ok(())
}

#[test]
fn inaccessible_listing_is_a_directory_error() -> Result<(), Box<dyn Error>> {
    let temporary = tempfile::tempdir()?;
    let path = temporary.path().join("file");
    fs::write(&path, b"not a directory")?;
    let directory = SnapshotDir::new(path);
    assert!(matches!(
        directory.entries(),
        Err(SnapshotDirError::Io { .. })
    ));
    assert!(matches!(
        directory.create(),
        Err(SnapshotDirError::Io { .. })
    ));
    Ok(())
}

#[cfg(unix)]
#[test]
fn follows_links_and_retains_broken_link_errors() -> Result<(), Box<dyn Error>> {
    use std::os::unix::fs::symlink;

    let temporary = tempfile::tempdir()?;
    let directory = SnapshotDir::new(temporary.path());
    let target = temporary.path().join("target");
    fs::write(&target, b"snapshot")?;
    symlink(&target, temporary.path().join("linked.adm"))?;
    symlink(temporary.path(), temporary.path().join("directory.adm"))?;
    symlink(
        temporary.path().join("missing"),
        temporary.path().join("broken.adm"),
    )?;
    let entries = directory.entries()?;
    let mut entries = entries.iter();
    let broken = entries.next().ok_or("missing broken link")?;
    assert_eq!(broken.file_stem(), Some(OsStr::new("broken")));
    assert!(broken.path().is_some());
    assert!(broken.error().is_some());
    assert_eq!(broken.byte_len(), None);
    assert_eq!(broken.modified(), None);
    let linked = entries.next().ok_or("missing linked snapshot")?;
    assert_eq!(linked.file_stem(), Some(OsStr::new("linked")));
    assert_eq!(linked.byte_len(), Some(8));
    assert_eq!(linked.modified(), Some(fs::metadata(target)?.modified()?));
    assert!(linked.error().is_none());
    assert!(entries.next().is_none());
    Ok(())
}
