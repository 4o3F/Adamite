//! Public thread bounds, cross-thread scan output and concurrent snapshot reads.

use adamite_core::{
    scan::{ScanConfig, ScanOutput, ScanStats, ScanStep, Scanner},
    snapshot::{NodeId, Snapshot},
};
use adamite_fixtures::Fixture;
use std::{
    error::Error,
    path::Path,
    sync::{Arc, Barrier},
    thread,
};

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

fn assert_send<T: Send>() {}
fn assert_send_sync<T: Send + Sync>() {}

#[test]
fn public_thread_safety_bounds() {
    assert_send::<Scanner>();
    assert_send::<ScanOutput>();
    assert_send::<ScanStats>();
    assert_send_sync::<Snapshot>();
}

fn scan_image(mut scanner: Scanner, image: &[u8]) -> TestResult<ScanOutput> {
    loop {
        match scanner.step()? {
            ScanStep::Need(request) => {
                let start = usize::try_from(request.offset)?;
                let end = start
                    .checked_add(request.len)
                    .ok_or("read range overflow")?;
                scanner.feed(
                    request.id,
                    image.get(start..end).ok_or("truncated fixture")?,
                )?;
            }
            ScanStep::Progress(_) => {}
            ScanStep::Done(output) => return Ok(output),
            _ => return Err("unsupported scan step".into()),
        }
    }
}

fn compare_reads(actual: &Snapshot, expected: &Snapshot) -> TestResult {
    assert_eq!(actual.len(), expected.len());
    assert_eq!(actual.is_empty(), expected.is_empty());
    assert_eq!(actual.volume(), expected.volume());
    for raw in 0..u32::try_from(expected.len())? {
        let id = NodeId::from_raw(raw);
        assert_eq!(actual.node(id), expected.node(id), "node {raw}");
        assert_eq!(actual.children(id), expected.children(id), "children {raw}");
        assert_eq!(actual.name(id), expected.name(id), "name {raw}");
        assert_eq!(actual.path(id)?, expected.path(id)?, "path {raw}");
    }
    assert_eq!(actual.top_files(), expected.top_files());
    assert_eq!(actual.ext_stats(), expected.ext_stats());
    Ok(())
}

#[test]
fn complete_scan_crosses_threads_and_snapshot_reads_are_shared() -> TestResult {
    let fixture = Fixture::load(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/ntfs/basic-4k.json"),
    )?;
    let baseline = scan_image(Scanner::new(ScanConfig::default())?, fixture.image())?;
    assert!(!baseline.snapshot.is_empty());

    let image = fixture.image().to_vec();
    let scanner = Scanner::new(ScanConfig::default())?;
    let worker = thread::spawn(move || scan_image(scanner, &image));
    let output = worker.join().map_err(|_| "scan worker panicked")??;
    assert_eq!(output, baseline);

    let snapshot = Arc::new(output.snapshot);
    let expected = Arc::new(baseline.snapshot);
    const READERS: usize = 4;
    let barrier = Arc::new(Barrier::new(READERS));
    let mut readers = Vec::new();
    for _ in 0..READERS {
        let snapshot = Arc::clone(&snapshot);
        let expected = Arc::clone(&expected);
        let barrier = Arc::clone(&barrier);
        readers.push(thread::spawn(move || {
            barrier.wait();
            compare_reads(&snapshot, &expected)
        }));
    }
    for reader in readers {
        reader.join().map_err(|_| "snapshot reader panicked")??;
    }
    Ok(())
}
