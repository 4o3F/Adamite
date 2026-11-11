//! Complete scan equivalence against an independent immediate-response loop.

use adamite_core::scan::{ScanConfig, ScanContext, ScanOutput, ScanStep, Scanner};
use adamite_fixtures::Fixture;
use adamite_io::{
    driver::SyncDriver,
    reader::{ImageReader, SliceReader, VolumeReader},
};
use std::{error::Error, path::Path};

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

fn baseline(config: ScanConfig, image: &[u8]) -> TestResult<ScanOutput> {
    let mut scanner = Scanner::new(config)?;
    loop {
        match scanner.step()? {
            ScanStep::Need(request) => {
                let start = usize::try_from(request.offset)?;
                let end = start
                    .checked_add(request.len)
                    .ok_or("read range overflow")?;
                let bytes = image.get(start..end).ok_or("truncated fixture")?;
                scanner.feed(request.id, bytes)?;
            }
            ScanStep::Progress(_) => {}
            ScanStep::Done(output) => return Ok(output),
            _ => return Err("unsupported scan step".into()),
        }
    }
}

#[test]
fn slice_and_image_scans_match_baseline_for_all_fixtures_and_configs() -> TestResult {
    for name in ["basic-4k", "frag-512", "4kn-64k"] {
        let fixture = Fixture::load(
            &Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(format!("../../tests/fixtures/ntfs/{name}.json")),
        )?;
        let image_path =
            Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("equivalence-{name}.img"));
        std::fs::write(&image_path, fixture.image())?;
        let context = ScanContext {
            label: Some("fixture volume".to_owned()),
            scanned_at_filetime: Some(133_000_000_000_000_000),
            free_bytes: Some(1_048_576),
        };
        let default_config = ScanConfig {
            context: context.clone(),
            ..ScanConfig::default()
        };
        let expected = baseline(default_config.clone(), fixture.image())?;
        assert!(!expected.snapshot.is_empty(), "fixture {name}");
        for config in [
            default_config,
            ScanConfig {
                max_outstanding: 4,
                chunk_size: 512,
                context: context.clone(),
            },
            ScanConfig {
                max_outstanding: 1,
                chunk_size: 4096,
                context: context.clone(),
            },
            ScanConfig {
                max_outstanding: 2,
                chunk_size: 16_777_216,
                context: context.clone(),
            },
        ] {
            let immediate = baseline(config.clone(), fixture.image())?;
            assert_eq!(
                immediate, expected,
                "baseline fixture {name}, config {config:?}"
            );
            let driver = SyncDriver::new(config.clone())?;
            assert_eq!(driver.config(), &config);
            let mut reader = SliceReader::new(fixture.image());
            let reader: &mut dyn VolumeReader = &mut reader;
            assert_eq!(
                driver.run(reader)?,
                immediate,
                "fixture {name}, config {config:?}"
            );
            assert_eq!(
                driver.run(reader)?,
                immediate,
                "reused driver for {name}, config {config:?}"
            );
            let mut reader = ImageReader::open(&image_path)?;
            assert_eq!(
                driver.run(&mut reader)?,
                immediate,
                "image fixture {name}, config {config:?}"
            );
        }
    }
    Ok(())
}

#[derive(Debug)]
struct RecordingReader<'a> {
    inner: SliceReader<'a>,
    requests: Vec<(u64, usize)>,
}

impl VolumeReader for RecordingReader<'_> {
    fn len_bytes(&self) -> u64 {
        self.inner.len_bytes()
    }

    fn read_exact_at(
        &mut self,
        offset: u64,
        buffer: &mut [u8],
    ) -> Result<(), adamite_io::error::VolumeError> {
        self.requests.push((offset, buffer.len()));
        self.inner.read_exact_at(offset, buffer)
    }
}

#[test]
fn limits_control_observed_reads_and_preserve_snapshot_bytes() -> TestResult {
    use adamite_io::limits::ScanProfile;
    use std::num::NonZeroUsize;

    let fixture = Fixture::load(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/ntfs/basic-4k.json"),
    )?;
    let expected = baseline(ScanConfig::default(), fixture.image())?;
    for chunk in [512, 1024, 4096, 16 << 20] {
        for depth in [1, 2, 5] {
            let mut limits = ScanProfile::Max.limits();
            limits.chunk_size = chunk;
            limits.io_depth = NonZeroUsize::new(depth).ok_or("depth")?;
            limits.threads = NonZeroUsize::new(16).ok_or("threads")?;
            let driver = SyncDriver::with_limits(limits, ScanContext::default())?;
            assert_eq!(driver.config().max_outstanding, depth);
            let mut reader = RecordingReader {
                inner: SliceReader::new(fixture.image()),
                requests: Vec::new(),
            };
            let actual = driver.run(&mut reader)?;
            assert_eq!(actual.to_bytes()?, expected.to_bytes()?);
            assert!(!reader.requests.is_empty());
            assert!(reader.requests.iter().all(|(_, len)| *len <= chunk));
            if chunk <= 4096 {
                assert!(reader.requests.iter().any(|(_, len)| *len == chunk));
            }
        }
    }
    Ok(())
}
