//! Source detection, snapshot-only loading and observed scans through the public API.

use adamite_access::{Source, SourceError};
use adamite_core::scan::ScanOutput;
use adamite_fixtures::Fixture;
use adamite_io::limits::ScanLimits;
use std::{error::Error, fs, io, path::Path};

type TestResult = Result<(), Box<dyn Error>>;

#[test]
fn snapshot_only_detection_uses_extension_or_complete_magic() -> TestResult {
    let directory = tempfile::tempdir()?;
    for (name, bytes, snapshot) in [
        ("damaged.AdM", b"bad".as_slice(), true),
        ("empty.adm", b"".as_slice(), true),
        ("magic.bin", ScanOutput::ADM_MAGIC.as_slice(), true),
        ("partial.bin", b"ADAMITE".as_slice(), false),
        ("image.img", b"not an adm snapshot".as_slice(), false),
        ("empty.img", b"".as_slice(), false),
    ] {
        let path = directory.path().join(name);
        fs::write(&path, bytes)?;
        let source = Source::open(&path)?;
        assert_eq!(source.is_snapshot(), snapshot, "{name}");
        assert_eq!(source.path(), path);
        match Source::open_snapshot(&path) {
            Ok(source) => {
                assert!(snapshot, "unexpected snapshot: {name}");
                assert!(source.is_snapshot());
                assert_eq!(source.path(), path);
                let mut observed = false;
                let error = source
                    .load_observed(&ScanLimits::default(), |_| observed = true)
                    .expect_err("truncated snapshots must fail to decode");
                assert!(
                    matches!(
                        error,
                        SourceError::Persist {
                            operation: "decode",
                            ..
                        }
                    ),
                    "{name}: {error}"
                );
                assert!(!observed, "snapshot decoding must not scan: {name}");
            }
            Err(error) => {
                assert!(!snapshot, "snapshot rejected: {name}: {error}");
                assert!(matches!(error, SourceError::UnsupportedSource));
            }
        }
    }
    for drive in ["C:", "z:"] {
        assert!(matches!(
            Source::open(Path::new(drive))?,
            Source::Drive { .. }
        ));
        assert!(matches!(
            Source::open_snapshot(Path::new(drive)),
            Err(SourceError::UnsupportedSource)
        ));
    }
    Ok(())
}

#[test]
fn missing_sources_preserve_operation_path_and_io_cause() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("missing.adm");
    for error in [
        Source::open(&path).expect_err("missing source"),
        Source::open_snapshot(&path).expect_err("missing snapshot"),
    ] {
        assert_eq!(
            error.to_string(),
            format!("cannot open source {}", path.display())
        );
        assert!(error.source().is_some());
        let SourceError::Io {
            operation,
            path: actual,
            source,
        } = error
        else {
            return Err("expected source I/O error".into());
        };
        assert_eq!(operation, "open source");
        assert_eq!(actual, path);
        assert_eq!(source.kind(), io::ErrorKind::NotFound);
    }
    Ok(())
}

#[test]
fn image_scan_reports_progress_and_snapshot_reload_preserves_output() -> TestResult {
    let directory = tempfile::tempdir()?;
    let fixture = Fixture::load(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/ntfs/basic-4k.json"),
    )?;
    let image = directory.path().join("source.img");
    fs::write(&image, fixture.image())?;
    assert!(matches!(
        Source::open_snapshot(&image),
        Err(SourceError::UnsupportedSource)
    ));
    let mut observed = false;
    let output =
        Source::open(&image)?.load_observed(&ScanLimits::default(), |_| observed = true)?;
    assert!(observed, "image scans must deliver progress");
    let bytes = output.to_bytes()?;
    for name in ["snapshot.adm", "snapshot.bin"] {
        let path = directory.path().join(name);
        fs::write(&path, &bytes)?;
        let mut observed = false;
        let output = Source::open_snapshot(&path)?
            .load_observed(&ScanLimits::default(), |_| observed = true)?;
        assert!(!observed, "snapshot loads do not report scan progress");
        assert_eq!(output.to_bytes()?, bytes, "{name}");
    }
    let limits = ScanLimits {
        chunk_size: 1,
        ..ScanLimits::default()
    };
    let error = Source::open(&image)?
        .load_observed(&limits, |_| {})
        .expect_err("invalid limits must fail");
    assert!(matches!(error, SourceError::Volume { path, drive: false, .. } if path == image));
    Ok(())
}

#[cfg(not(windows))]
#[test]
fn raw_drives_require_windows_without_progress() -> TestResult {
    let mut observed = false;
    let error = Source::open(Path::new("C:"))?
        .load_observed(&ScanLimits::default(), |_| observed = true)
        .expect_err("raw drive outside Windows");
    assert!(matches!(error, SourceError::RawVolumesRequireWindows));
    assert_eq!(error.to_string(), "raw volumes require Windows");
    assert!(!observed);
    Ok(())
}
