//! Public thread bounds and complete scan execution on a worker thread.

use adamite_core::scan::{ScanConfig, ScanContext};
use adamite_fixtures::Fixture;
use adamite_io::{driver::SyncDriver, error::VolumeError, reader::SliceReader};
use std::{error::Error, path::Path, thread};

type TestResult = Result<(), Box<dyn Error + Send + Sync>>;

fn assert_send<T: Send>() {}
fn assert_send_sync<T: Send + Sync>() {}

#[test]
fn public_thread_safety_bounds() {
    assert_send::<SliceReader<'static>>();
    assert_send::<SyncDriver>();
    assert_send_sync::<VolumeError>();
}

#[test]
fn complete_scan_matches_across_threads() -> TestResult {
    let fixture = Fixture::load(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/ntfs/basic-4k.json"),
    )?;
    let driver = SyncDriver::new(ScanConfig {
        max_outstanding: 4,
        chunk_size: 512,
        context: ScanContext {
            label: Some("thread fixture".to_owned()),
            scanned_at_filetime: Some(133_000_000_000_000_000),
            free_bytes: Some(1_048_576),
        },
    })?;
    let baseline = driver.run(&mut SliceReader::new(fixture.image()))?;
    assert!(!baseline.snapshot.is_empty());
    let image = fixture.image().to_vec();
    let worker = thread::spawn(move || driver.run(&mut SliceReader::new(&image)));
    let output = worker.join().map_err(|_| "scan worker panicked")??;
    assert_eq!(output, baseline);
    Ok(())
}
