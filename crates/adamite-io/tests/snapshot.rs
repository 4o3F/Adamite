//! Shared snapshot file path, fragmented reads and typed error preservation.

use adamite_core::{scan::ScanOutput, snapshot::PersistError};
use adamite_fixtures::Fixture;
use adamite_io::{
    driver::SyncDriver,
    reader::SliceReader,
    snapshot::{SnapshotIoError, SnapshotReader, SnapshotWriter},
};
use std::{
    error::Error,
    fs::{self, File},
    io::{self, Cursor, Read, Seek, SeekFrom},
    path::Path,
};

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

struct Fragmented<'a> {
    inner: Cursor<&'a [u8]>,
    limit: usize,
}

impl Read for Fragmented<'_> {
    fn read(&mut self, target: &mut [u8]) -> io::Result<usize> {
        let len = target.len().min(self.limit);
        self.inner.read(&mut target[..len])
    }
}

impl Seek for Fragmented<'_> {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.inner.seek(position)
    }
}

fn output() -> TestResult<ScanOutput> {
    let fixture = Fixture::load(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/ntfs/basic-4k.json"),
    )?;
    Ok(SyncDriver::new(Default::default())?.run(&mut SliceReader::new(fixture.image()))?)
}

#[test]
fn shared_adapter_preserves_bytes_across_fragmented_reads() -> TestResult {
    let output = output()?;
    let expected = output.to_bytes()?;
    let mut target = Cursor::new(Vec::new());
    let len = SnapshotWriter::write(&output, &mut target)?;
    assert_eq!(len, expected.len());
    assert_eq!(target.position(), u64::try_from(len)?);
    assert_eq!(target.get_ref(), &expected);
    for limit in [1, 7, 4096, expected.len()] {
        let mut source = Fragmented {
            inner: Cursor::new(expected.as_slice()),
            limit,
        };
        assert_eq!(
            SnapshotReader::read(&mut source)?,
            output,
            "block size {limit}"
        );
    }
    Ok(())
}

#[test]
fn file_round_trip_closes_handles_before_removal() -> TestResult {
    let output = output()?;
    let path = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("snapshot-round-trip-{}.adm", std::process::id()));
    let mut file = File::create(&path)?;
    SnapshotWriter::write(&output, &mut file)?;
    file.sync_all()?;
    drop(file);
    let mut file = File::open(&path)?;
    assert_eq!(SnapshotReader::read(&mut file)?, output);
    drop(file);
    fs::remove_file(path)?;
    Ok(())
}

#[test]
fn adapter_preserves_format_errors() -> TestResult {
    let original = output()?.to_bytes()?;
    for cut in [0, 7, 63, 480, original.len() - 1] {
        let bytes = &original[..cut];
        let expected = ScanOutput::from_bytes(bytes).expect_err("truncated snapshot");
        let error = SnapshotReader::read(&mut Cursor::new(bytes)).expect_err("truncated file");
        let SnapshotIoError::Persist { source } = error else {
            return Err("lost codec error".into());
        };
        assert_eq!(format!("{source:?}"), format!("{expected:?}"));
    }
    let mut damaged = original;
    damaged[32] ^= 1;
    assert!(matches!(
        SnapshotReader::read(&mut Cursor::new(damaged)),
        Err(SnapshotIoError::Persist {
            source: PersistError::ChecksumMismatch { .. }
        })
    ));
    Ok(())
}
