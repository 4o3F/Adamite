//! Reader failures, exact-read bounds and wrapped core error chains.

use adamite_core::{
    error::ScanError,
    scan::{ConfigError, ScanConfig},
};
use adamite_fixtures::Fixture;
use adamite_io::{
    driver::SyncDriver,
    error::VolumeError,
    reader::{SliceReader, VolumeReader},
};
use std::{error::Error, io, path::Path};

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

#[derive(Clone, Copy)]
enum Fault {
    Short,
    Read,
}

struct FaultReader<'a> {
    inner: SliceReader<'a>,
    reads: Vec<(u64, usize)>,
    fail_at: Option<(usize, Fault)>,
}

impl<'a> FaultReader<'a> {
    fn new(image: &'a [u8], fail_at: Option<(usize, Fault)>) -> Self {
        Self {
            inner: SliceReader::new(image),
            reads: Vec::new(),
            fail_at,
        }
    }
}

impl VolumeReader for FaultReader<'_> {
    fn len_bytes(&self) -> u64 {
        self.inner.len_bytes()
    }

    fn read_exact_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), VolumeError> {
        self.reads.push((offset, buf.len()));
        match self.fail_at {
            Some((n, Fault::Short)) if n == self.reads.len() => Err(VolumeError::ShortRead {
                offset,
                expected: buf.len(),
                actual: 0,
            }),
            Some((n, Fault::Read)) if n == self.reads.len() => Err(VolumeError::Read {
                offset,
                len: buf.len(),
                source: io::Error::other("injected read failure"),
            }),
            _ => self.inner.read_exact_at(offset, buf),
        }
    }
}

fn fixture() -> TestResult<Fixture> {
    Ok(Fixture::load(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/ntfs/basic-4k.json"),
    )?)
}

#[test]
fn read_failures_preserve_first_second_and_last_request_details() -> TestResult {
    let fixture = fixture()?;
    let driver = SyncDriver::new(ScanConfig::default())?;
    let mut complete = FaultReader::new(fixture.image(), None);
    driver.run(&mut complete)?;
    let last = complete.reads.len();
    assert!(last > 2);
    for n in [1, 2, last] {
        let index = n.checked_sub(1).ok_or("invalid read index")?;
        let (expected_offset, expected_len) =
            *complete.reads.get(index).ok_or("missing baseline read")?;
        for fault in [Fault::Short, Fault::Read] {
            let mut reader = FaultReader::new(fixture.image(), Some((n, fault)));
            let error = driver
                .run(&mut reader)
                .err()
                .ok_or("injected fault was ignored")?;
            assert_eq!(reader.reads.len(), n);
            match fault {
                Fault::Short => {
                    assert!(
                        matches!(error, VolumeError::ShortRead { offset, expected, actual: 0 }
                        if offset == expected_offset && expected == expected_len)
                    );
                    assert!(error.source().is_none());
                }
                Fault::Read => {
                    assert!(matches!(error, VolumeError::Read { offset, len, .. }
                        if offset == expected_offset && len == expected_len));
                    let source = error.source().ok_or("read source was lost")?;
                    let source = source
                        .downcast_ref::<io::Error>()
                        .ok_or("wrong read source type")?;
                    assert_eq!(source.kind(), io::ErrorKind::Other);
                    assert_eq!(source.to_string(), "injected read failure");
                    assert!(source.source().is_none());
                }
            }
        }
    }
    Ok(())
}

#[test]
fn invalid_configuration_preserves_core_source() -> TestResult {
    for (max_outstanding, chunk_size) in [(0, 512), (1, 513), (usize::MAX, 512)] {
        let error = SyncDriver::new(ScanConfig {
            max_outstanding,
            chunk_size,
            ..ScanConfig::default()
        })
        .err()
        .ok_or("invalid configuration accepted")?;
        let VolumeError::Config {
            source: ref config_error,
        } = error
        else {
            return Err("wrong configuration error variant".into());
        };
        match (max_outstanding, chunk_size) {
            (0, _) => assert!(matches!(config_error, ConfigError::ZeroOutstanding)),
            (_, 513) => assert!(matches!(
                config_error,
                ConfigError::InvalidChunkSize { chunk_size: 513 }
            )),
            _ => assert!(matches!(config_error, ConfigError::BufferSizeOverflow {
                max_outstanding: actual, chunk_size: 512,
            } if *actual == usize::MAX)),
        }
        let source = error.source().ok_or("configuration source was lost")?;
        assert_eq!(source.downcast_ref::<ConfigError>(), Some(config_error));
        assert!(source.source().is_none());
    }
    Ok(())
}

#[test]
fn blank_sector_preserves_scan_and_geometry_sources() -> TestResult {
    let driver = SyncDriver::new(ScanConfig::default())?;
    let error = driver
        .run(&mut SliceReader::new(&[0u8; 512]))
        .err()
        .ok_or("blank sector accepted")?;
    assert!(matches!(
        error,
        VolumeError::Scan {
            source: ScanError::Geometry { .. }
        }
    ));
    let source = error.source().ok_or("scan source was lost")?;
    let scan_error = source
        .downcast_ref::<ScanError>()
        .ok_or("wrong scan source type")?;
    let ScanError::Geometry { source: geometry } = scan_error else {
        return Err("wrong scan error variant".into());
    };
    let source = source.source().ok_or("geometry source was lost")?;
    assert_eq!(source.to_string(), geometry.to_string());
    assert!(source.source().is_none());
    Ok(())
}

#[test]
fn truncated_slice_reports_actual_volume_length_before_reading() -> TestResult {
    let fixture = fixture()?;
    let image = fixture
        .image()
        .get(..512)
        .ok_or("invalid truncated range")?;
    let driver = SyncDriver::new(ScanConfig::default())?;
    let mut complete = FaultReader::new(fixture.image(), None);
    driver.run(&mut complete)?;
    let (expected_offset, expected_len) =
        *complete.reads.get(1).ok_or("missing record-zero read")?;
    let mut reader = FaultReader::new(image, None);
    let error = driver
        .run(&mut reader)
        .err()
        .ok_or("truncated volume accepted")?;
    let VolumeError::OutOfBounds {
        offset,
        len,
        volume_len,
    } = error
    else {
        return Err("wrong truncated volume error".into());
    };
    assert_eq!(offset, expected_offset);
    assert_eq!(len, expected_len);
    assert_eq!(volume_len, u64::try_from(image.len())?);
    assert!(
        offset
            .checked_add(u64::try_from(len)?)
            .is_none_or(|end| end > volume_len)
    );
    assert!(!reader.reads.contains(&(offset, len)));
    assert_eq!(reader.reads, [(0, 512)]);
    assert!(error.source().is_none());
    Ok(())
}

#[test]
fn slice_reads_fill_exactly_and_reject_invalid_ranges_without_copying() -> TestResult {
    let bytes = [1, 2, 3, 4];
    let mut reader = SliceReader::new(&bytes);
    assert_eq!(reader.len_bytes(), 4);
    let mut buffer = [0; 2];
    reader.read_exact_at(2, &mut buffer)?;
    assert_eq!(buffer, [3, 4]);
    reader.read_exact_at(4, &mut [])?;
    for offset in [3, 4, u64::MAX] {
        let mut buffer = [9; 2];
        let error = reader
            .read_exact_at(offset, &mut buffer)
            .err()
            .ok_or("invalid slice read accepted")?;
        assert!(matches!(error, VolumeError::OutOfBounds {
            offset: actual, len: 2, volume_len: 4,
        } if actual == offset));
        assert_eq!(buffer, [9, 9]);
        assert!(error.source().is_none());
    }
    Ok(())
}
