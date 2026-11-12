use crate::{McpError, SnapshotCache, SnapshotOptions, SnapshotStore};
use adamite_core::scan::ScanContext;
use adamite_fixtures::Fixture;
use adamite_io::{driver::SyncDriver, limits::ScanLimits, reader::ImageReader};
use std::{
    error::Error,
    fs::{self, File, FileTimes},
    io,
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

pub(super) struct Case {
    directory: tempfile::TempDir,
    bytes: Vec<u8>,
}

impl Case {
    pub(super) fn new() -> TestResult<Self> {
        Self::with_label("test")
    }

    fn with_label(label: &str) -> TestResult<Self> {
        let directory = tempfile::tempdir()?;
        let fixture = Fixture::load(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/ntfs/basic-4k.json"),
        )?;
        let image = directory.path().join("source.img");
        fs::write(&image, fixture.image())?;
        let mut reader = ImageReader::open(&image)?;
        let output = SyncDriver::with_limits(
            ScanLimits::default(),
            ScanContext {
                label: Some(label.into()),
                scanned_at_filetime: Some(116_444_736_000_000_000),
                ..ScanContext::default()
            },
        )?
        .run(&mut reader)?;
        Ok(Self {
            directory,
            bytes: output.to_bytes()?,
        })
    }

    pub(super) fn snapshot(&self, name: &str) -> TestResult<PathBuf> {
        let path = self.directory.path().join(name);
        fs::write(&path, &self.bytes)?;
        Ok(path)
    }

    fn store(&self) -> TestResult<SnapshotStore> {
        Ok(SnapshotStore::new(SnapshotOptions {
            snapshot_dir: Some(self.directory.path().to_owned()),
            ..SnapshotOptions::default()
        })?)
    }

    pub(super) fn wait_until(condition: impl Fn() -> bool) -> TestResult {
        let start = Instant::now();
        while !condition() {
            if start.elapsed() > Duration::from_secs(5) {
                return Err("workers did not reach the pending load within five seconds".into());
            }
            thread::sleep(Duration::from_millis(1));
        }
        Ok(())
    }
}

#[test]
fn discovery_refreshes_metadata_merges_explicit_files_and_adds_cached_time() -> TestResult {
    let case = Case::new()?;
    let saved = case.snapshot("C.adm")?;
    let elsewhere = tempfile::tempdir()?;
    let explicit = elsewhere.path().join("D.adm");
    fs::write(&explicit, b"metadata only; deliberately not a snapshot")?;
    let missing = elsewhere.path().join("missing.adm");
    fs::create_dir(case.directory.path().join("nested.adm"))?;
    fs::write(case.directory.path().join("ignored.txt"), b"ignored")?;
    let store = SnapshotStore::new(SnapshotOptions {
        snapshots: vec![saved.clone(), explicit.clone(), missing.clone()],
        snapshot_dir: Some(case.directory.path().to_owned()),
        ..SnapshotOptions::default()
    })?;
    let entries = store.snapshots()?;
    assert_eq!(entries.len(), 3);
    let row = entries
        .iter()
        .find(|entry| entry.path.as_deref() == Some(&saved))
        .ok_or("missing C row")?;
    assert_eq!(row.name.as_deref(), saved.file_name());
    assert_eq!(row.byte_len, Some(fs::metadata(&saved)?.len()));
    assert_eq!(row.modified, Some(fs::metadata(&saved)?.modified()?));
    assert_eq!(row.scan_time.0, None);
    assert!(row.error.is_none());
    assert!(
        entries
            .iter()
            .all(|entry| entry.path.as_ref().is_some_and(|path| path.is_absolute()))
    );
    let broken = entries
        .iter()
        .find(|entry| entry.path.as_deref() == Some(&missing))
        .ok_or("missing error row")?;
    assert!(broken.error.is_some());
    assert!(broken.byte_len.is_none());
    let output = store.load(Some("c"))?;
    let entries = store.snapshots()?;
    let row = entries
        .iter()
        .find(|entry| entry.path.as_deref() == Some(&saved))
        .ok_or("missing loaded row")?;
    assert_eq!(
        row.scan_time.0,
        output.snapshot.volume().context().scanned_at_filetime
    );
    assert!(row.scan_time.0.is_some());
    fs::write(&saved, b"replaced")?;
    let entries = store.snapshots()?;
    assert_eq!(
        entries
            .iter()
            .find(|entry| entry.path.as_deref() == Some(&saved))
            .ok_or("missing replaced row")?
            .scan_time
            .0,
        None
    );
    case.snapshot("fresh.AdM")?;
    assert_eq!(store.snapshots()?.len(), 4);
    Ok(())
}

#[test]
fn missing_override_stays_empty_and_is_rediscovered_when_created() -> TestResult {
    let directory = tempfile::tempdir()?;
    let missing = directory.path().join("missing");
    let store = SnapshotStore::new(SnapshotOptions {
        snapshot_dir: Some(missing.clone()),
        ..SnapshotOptions::default()
    })?;
    assert!(store.snapshots()?.is_empty());
    assert!(!missing.exists());
    assert_eq!(
        store.select(None).expect_err("empty listing").code(),
        "no_snapshot"
    );
    fs::create_dir(&missing)?;
    fs::write(missing.join("C.adm"), b"listed without decoding")?;
    assert_eq!(store.snapshots()?.len(), 1);
    Ok(())
}

#[cfg(unix)]
#[test]
fn discovery_follows_file_links_keeps_broken_links_and_skips_directory_links() -> TestResult {
    use std::os::unix::fs::symlink;
    let case = Case::new()?;
    let target = case.snapshot("target.bin")?;
    let alias = case.directory.path().join("C.adm");
    symlink(&target, &alias)?;
    symlink(
        case.directory.path().join("absent"),
        case.directory.path().join("broken.adm"),
    )?;
    let nested = case.directory.path().join("nested");
    fs::create_dir(&nested)?;
    fs::write(nested.join("hidden.adm"), b"ignored")?;
    symlink(&nested, case.directory.path().join("directory.adm"))?;
    let store = case.store()?;
    let rows = store.snapshots()?;
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter()
            .any(|row| row.error.is_some() && row.byte_len.is_none())
    );
    assert!(!store.load(Some("C:"))?.snapshot.is_empty());
    Ok(())
}

#[test]
fn selection_handles_paths_volumes_and_zero_one_or_two_discovered_files() -> TestResult {
    let case = Case::new()?;
    let store = case.store()?;
    assert_eq!(
        store.select(None).expect_err("empty listing").code(),
        "no_snapshot"
    );
    assert_eq!(
        store.select(Some("C:")).expect_err("missing C").code(),
        "no_snapshot"
    );
    let saved = case.snapshot("C.adm")?;
    assert_eq!(store.select(None)?, saved);
    assert_eq!(store.select(Some("C:"))?, saved);
    assert_eq!(store.select(Some("c"))?, saved);
    assert_eq!(store.select(saved.to_str())?, saved);
    let outside = tempfile::tempdir()?;
    let unlisted = outside.path().join("archive.adm");
    fs::write(&unlisted, &case.bytes)?;
    assert_eq!(store.select(unlisted.to_str())?, unlisted);
    let d = case.snapshot("D.AdM")?;
    match store
        .select(None)
        .expect_err("two snapshots require selection")
    {
        McpError::AmbiguousSnapshot { candidates } => {
            assert_eq!(candidates, [saved.clone(), d.clone()])
        }
        error => return Err(error.into()),
    }
    assert_eq!(store.select(Some("d:"))?, d);
    let image = case.directory.path().join("source.img");
    assert_eq!(
        store
            .select(image.to_str())
            .expect_err("image rejected")
            .code(),
        "unsupported_source"
    );
    // A valid ADM header without the extension remains unsupported for MCP selection.
    let magic = case.snapshot("header.bin")?;
    assert_eq!(
        store
            .load(magic.to_str())
            .expect_err("non-adm rejected")
            .code(),
        "unsupported_source"
    );
    Ok(())
}

#[test]
fn volume_ambiguity_includes_candidates_from_explicit_files() -> TestResult {
    let case = Case::new()?;
    let first = case.snapshot("C.adm")?;
    let elsewhere = tempfile::tempdir()?;
    let second = elsewhere.path().join("c.AdM");
    fs::write(&second, &case.bytes)?;
    let store = SnapshotStore::new(SnapshotOptions {
        snapshots: vec![second.clone()],
        snapshot_dir: Some(case.directory.path().to_owned()),
        ..SnapshotOptions::default()
    })?;
    match store.select(Some("c:")).expect_err("ambiguous volume") {
        error @ McpError::AmbiguousSnapshot { .. } => {
            assert_eq!(error.code(), "ambiguous_snapshot");
            assert!(error.next_step().contains(&first.display().to_string()));
            assert!(error.next_step().contains(&second.display().to_string()));
        }
        error => return Err(error.into()),
    }
    let image_store = SnapshotStore::new(SnapshotOptions {
        snapshots: vec![case.snapshot("C.img")?],
        snapshot_dir: Some(elsewhere.path().join("missing")),
        ..SnapshotOptions::default()
    })?;
    assert_eq!(
        image_store
            .select(Some("C"))
            .expect_err("no .adm for C")
            .code(),
        "no_snapshot"
    );
    Ok(())
}

#[test]
fn cache_hits_share_an_arc_and_mtime_only_changes_reload() -> TestResult {
    let case = Case::new()?;
    let path = case.snapshot("C.adm")?;
    let cache = SnapshotCache::default();
    let first = cache.load(&path)?;
    assert!(Arc::ptr_eq(&first, &cache.load(&path)?));
    let previous = fs::metadata(&path)?.modified()?;
    let modified = previous
        .checked_add(Duration::from_secs(5))
        .ok_or("mtime overflow")?;
    let file = File::options().write(true).open(&path)?;
    file.set_times(FileTimes::new().set_modified(modified))?;
    drop(file);
    let second = cache.load(&path)?;
    assert!(!Arc::ptr_eq(&first, &second));
    assert_eq!(*first, *second);
    assert!(Arc::ptr_eq(&second, &cache.load(&path)?));
    Ok(())
}

#[test]
fn length_change_invalidates_even_when_mtime_is_preserved_and_failure_retries() -> TestResult {
    let case = Case::new()?;
    let path = case.snapshot("C.adm")?;
    let cache = SnapshotCache::default();
    let first = cache.load(&path)?;
    let modified = fs::metadata(&path)?.modified()?;
    let mut damaged = case.bytes.clone();
    damaged.push(0);
    fs::write(&path, &damaged)?;
    let file = File::options().write(true).open(&path)?;
    file.set_times(FileTimes::new().set_modified(modified))?;
    drop(file);
    assert_eq!(
        cache.load(&path).expect_err("invalid length").code(),
        "corrupt_snapshot"
    );
    fs::write(&path, &case.bytes)?;
    let second = cache.load(&path)?;
    assert!(!Arc::ptr_eq(&first, &second));
    assert_eq!(*first, *second);
    Ok(())
}

#[test]
fn failed_decode_is_not_cached_even_with_identical_metadata_on_retry() -> TestResult {
    let case = Case::new()?;
    let path = case.snapshot("C.adm")?;
    let modified = fs::metadata(&path)?.modified()?;
    let mut damaged = case.bytes.clone();
    let first = damaged.first_mut().ok_or("empty fixture")?;
    *first = 0;
    fs::write(&path, damaged)?;
    let file = File::options().write(true).open(&path)?;
    file.set_times(FileTimes::new().set_modified(modified))?;
    drop(file);
    let cache = SnapshotCache::default();
    assert_eq!(
        cache.load(&path).expect_err("bad magic").code(),
        "corrupt_snapshot"
    );
    fs::write(&path, &case.bytes)?;
    let file = File::options().write(true).open(&path)?;
    file.set_times(FileTimes::new().set_modified(modified))?;
    drop(file);
    assert!(!cache.load(&path)?.snapshot.is_empty());
    Ok(())
}

#[test]
fn length_change_reloads_a_valid_replacement_with_the_same_mtime() -> TestResult {
    let case = Case::new()?;
    let replacement = Case::with_label("a longer replacement label")?;
    let path = case.snapshot("C.adm")?;
    let cache = SnapshotCache::default();
    let first = cache.load(&path)?;
    let modified = fs::metadata(&path)?.modified()?;
    assert_ne!(case.bytes.len(), replacement.bytes.len());
    fs::write(&path, &replacement.bytes)?;
    let file = File::options().write(true).open(&path)?;
    file.set_times(FileTimes::new().set_modified(modified))?;
    drop(file);
    let second = cache.load(&path)?;
    assert!(!Arc::ptr_eq(&first, &second));
    assert_eq!(
        second.snapshot.volume().context().label.as_deref(),
        Some("a longer replacement label")
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn canonical_paths_share_outputs_even_through_different_extensions() -> TestResult {
    let case = Case::new()?;
    let target = case.snapshot("target.bin")?;
    let first = case.directory.path().join("C.adm");
    let second = case.directory.path().join("D.adm");
    std::os::unix::fs::symlink(&target, &first)?;
    std::os::unix::fs::symlink(&target, &second)?;
    let cache = SnapshotCache::default();
    let output = cache.load(&first)?;
    assert!(Arc::ptr_eq(&output, &cache.load(&second)?));
    Ok(())
}

#[test]
fn lru_evicts_the_oldest_unused_output_and_preserves_access_order() -> TestResult {
    let case = Case::new()?;
    let c = case.snapshot("C.adm")?;
    let d = case.snapshot("D.adm")?;
    let e = case.snapshot("E.adm")?;
    let cache = SnapshotCache::default();
    let old_c = Arc::downgrade(&cache.load(&c)?);
    let old_d = Arc::downgrade(&cache.load(&d)?);
    drop(cache.load(&c)?);
    drop(cache.load(&e)?);
    assert!(old_c.upgrade().is_some());
    assert!(old_d.upgrade().is_none());
    Ok(())
}

#[test]
fn lru_skips_active_entries_and_trims_temporary_overflow_on_next_access() -> TestResult {
    let case = Case::new()?;
    let c = case.snapshot("C.adm")?;
    let d = case.snapshot("D.adm")?;
    let e = case.snapshot("E.adm")?;
    let cache = SnapshotCache::new(1);
    let held_c = cache.load(&c)?;
    let held_d = cache.load(&d)?;
    assert!(Arc::ptr_eq(&held_c, &cache.load(&c)?));
    assert!(Arc::ptr_eq(&held_d, &cache.load(&d)?));
    let weak_d = Arc::downgrade(&held_d);
    drop(held_d);
    let held_e = cache.load(&e)?;
    assert!(weak_d.upgrade().is_none());
    assert!(Arc::ptr_eq(&held_c, &cache.load(&c)?));
    let weak_c = Arc::downgrade(&held_c);
    drop(held_c);
    assert!(Arc::ptr_eq(&held_e, &cache.load(&e)?));
    assert!(weak_c.upgrade().is_none());
    Ok(())
}

#[test]
fn error_codes_and_next_steps_include_concrete_recovery_commands() -> TestResult {
    let case = Case::new()?;
    let store = case.store()?;
    let absent = store.select(None).expect_err("empty listing");
    assert!(absent.next_step().contains(&format!(
        "adamite scan C: -o \"{}\"",
        store.directory().join("C.adm").display()
    )));
    assert!(!absent.next_step().contains("--force"));
    case.snapshot("C.adm")?;
    assert!(absent.next_step().ends_with("--force"));
    let absent_d = store.select(Some("d:")).expect_err("missing D");
    assert!(absent_d.next_step().contains("scan_volume for D:"));
    assert!(absent_d.next_step().contains("adamite scan D:"));
    let denied = McpError::io("C.adm", io::Error::from(io::ErrorKind::PermissionDenied));
    assert_eq!(denied.code(), "permission_denied");
    assert!(denied.next_step().contains("Check access"));
    let missing = McpError::io("C.adm", io::Error::from(io::ErrorKind::NotFound));
    assert_eq!(missing.code(), "internal_error");
    let unsupported = store.select(Some("disk.img")).expect_err("image");
    assert!(unsupported.next_step().contains("adamite scan"));
    assert!(unsupported.next_step().contains("disk.img"));
    let damaged = case.directory.path().join("C.adm");
    fs::write(&damaged, b"bad")?;
    let corrupt = store.load(Some("C")).expect_err("bad snapshot");
    assert_eq!(corrupt.code(), "corrupt_snapshot");
    assert!(corrupt.next_step().contains("--force"));
    assert!(corrupt.next_step().contains(&damaged.display().to_string()));
    Ok(())
}

#[test]
fn cache_and_store_can_be_shared_between_blocking_workers() {
    struct Check<T>(T);
    impl<T: Send + Sync> Check<T> {
        const VALID: () = ();
    }
    let () = Check::<SnapshotCache>::VALID;
    let () = Check::<SnapshotStore>::VALID;
    let () = Check::<McpError>::VALID;
}
