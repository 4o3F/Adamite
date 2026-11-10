//! Public Boot driver and configuration contract.

use adamite_core::{
    error::ScanError,
    scan::{
        ConfigError, FeedError, ReadRequest, RequestId, ScanConfig, ScanContext, ScanPhase,
        ScanProgress, ScanStep, Scanner,
    },
};
use std::error::Error;

fn boot_request(scanner: &mut Scanner) -> Result<ReadRequest, Box<dyn Error>> {
    match scanner.step()? {
        ScanStep::Need(request) => Ok(request),
        other => Err(format!("expected Boot request, received {other:?}").into()),
    }
}

#[test]
fn protocol_config_defaults_and_context() -> Result<(), Box<dyn Error>> {
    let config = ScanConfig::default();
    assert_eq!(config.max_outstanding, 1);
    assert_eq!(config.chunk_size, 1_048_576);
    assert_eq!(config.context, ScanContext::default());
    config.validate()?;
    let config = ScanConfig {
        context: ScanContext {
            label: Some("test volume".into()),
            scanned_at_filetime: Some(123),
            free_bytes: Some(4096),
        },
        ..config
    };
    assert_eq!(Scanner::new(config.clone())?.config(), &config);
    Ok(())
}

#[test]
fn protocol_config_bounds() -> Result<(), Box<dyn Error>> {
    for chunk_size in [512, 1024, 16_777_216] {
        let config = ScanConfig {
            max_outstanding: 3,
            chunk_size,
            ..ScanConfig::default()
        };
        config.validate()?;
        Scanner::new(config)?;
    }
    for chunk_size in [0, 1, 511, 513, 1023, 16_777_728, usize::MAX] {
        let config = ScanConfig {
            chunk_size,
            ..ScanConfig::default()
        };
        assert!(matches!(
            config.validate(),
            Err(ConfigError::InvalidChunkSize { chunk_size: rejected }) if rejected == chunk_size
        ));
        assert!(matches!(
            Scanner::new(config),
            Err(ConfigError::InvalidChunkSize { .. })
        ));
    }
    let zero = ScanConfig {
        max_outstanding: 0,
        ..ScanConfig::default()
    };
    assert!(matches!(
        Scanner::new(zero),
        Err(ConfigError::ZeroOutstanding)
    ));
    let overflow = ScanConfig {
        max_outstanding: usize::MAX,
        chunk_size: 512,
        ..ScanConfig::default()
    };
    assert!(matches!(
        Scanner::new(overflow),
        Err(ConfigError::BufferSizeOverflow { .. })
    ));
    Ok(())
}

#[test]
fn protocol_boot_request_and_repeated_waiting() -> Result<(), Box<dyn Error>> {
    for max_outstanding in [1, 4] {
        let mut scanner = Scanner::new(ScanConfig {
            max_outstanding,
            ..ScanConfig::default()
        })?;
        let request = boot_request(&mut scanner)?;
        assert_eq!(request.offset, 0);
        assert_eq!(request.len, 512);
        assert_eq!(RequestId::from_raw(request.id.raw()), request.id);
        let waiting = ScanStep::Progress(ScanProgress {
            phase: ScanPhase::Boot,
            records_processed: 0,
            outstanding: 1,
            waiting_for_input: true,
        });
        for _ in 0..3 {
            assert_eq!(scanner.step()?, waiting);
        }
    }
    Ok(())
}

#[test]
fn protocol_rejected_feeds_leave_boot_request_pending() -> Result<(), Box<dyn Error>> {
    let mut scanner = Scanner::new(ScanConfig::default())?;
    let unknown = RequestId::from_raw(u64::MAX);
    assert!(matches!(
        scanner.feed(unknown, &[0; 512]),
        Err(FeedError::UnknownRequest { .. })
    ));
    let request = boot_request(&mut scanner)?;
    let waiting = scanner.step()?;
    assert!(matches!(
        scanner.feed(unknown, &[0; 512]),
        Err(FeedError::UnknownRequest { .. })
    ));
    assert_eq!(scanner.step()?, waiting);
    for bytes in [Vec::new(), vec![0; 511], vec![0; 513]] {
        assert!(matches!(
            scanner.feed(request.id, &bytes),
            Err(FeedError::LengthMismatch { expected: 512, actual, .. }) if actual == bytes.len()
        ));
        assert_eq!(scanner.step()?, waiting);
    }
    scanner.feed(request.id, &[0; 512])?;
    assert!(matches!(
        scanner.feed(request.id, &[1; 512]),
        Err(FeedError::DuplicateResponse { .. })
    ));
    assert!(matches!(scanner.step(), Err(ScanError::Geometry { .. })));
    Ok(())
}

#[test]
fn protocol_boot_terminal_error_is_permanent() -> Result<(), Box<dyn Error>> {
    let mut scanner = Scanner::new(ScanConfig::default())?;
    let request = boot_request(&mut scanner)?;
    scanner.feed(request.id, &vec![0; request.len])?;
    assert!(matches!(scanner.step(), Err(ScanError::Geometry { .. })));
    for _ in 0..3 {
        assert!(matches!(scanner.step(), Err(ScanError::Inactive)));
        assert!(matches!(
            scanner.feed(request.id, &[0; 512]),
            Err(FeedError::Inactive)
        ));
    }
    Ok(())
}
