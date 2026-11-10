//! Complete scan equivalence against an independent immediate-response loop.

use adamite_core::scan::{ScanConfig, ScanContext, ScanOutput, ScanStep, Scanner};
use adamite_fixtures::Fixture;
use adamite_io::{
    driver::SyncDriver,
    reader::{SliceReader, VolumeReader},
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
fn slice_scans_match_baseline_for_all_fixtures_and_configs() -> TestResult {
    for name in ["basic-4k", "frag-512", "4kn-64k"] {
        let fixture = Fixture::load(
            &Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(format!("../../tests/fixtures/ntfs/{name}.json")),
        )?;
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
        }
    }
    Ok(())
}
