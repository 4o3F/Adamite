//! Protocol phase regressions.

use super::support::{TestResult, boot};
use crate::scan::{FeedError, ScanConfig, ScanPhase, ScanProgress, ScanStep, Scanner};

#[test]
fn bootstrap_record0_short_boot_response_is_rejected() -> TestResult {
    let mut scanner = Scanner::new(ScanConfig::default())?;
    let ScanStep::Need(request) = scanner.step()? else {
        return Err("expected boot request".into());
    };
    assert!(matches!(
        scanner.feed(request.id, &[0; 511]),
        Err(FeedError::LengthMismatch { .. })
    ));
    let ScanStep::Progress(progress) = scanner.step()? else {
        return Err("expected waiting".into());
    };
    assert!(progress.waiting_for_input);
    scanner.feed(request.id, &boot().build())?;
    assert!(matches!(
        scanner.step()?,
        ScanStep::Progress(ScanProgress {
            phase: ScanPhase::Record0,
            ..
        })
    ));
    Ok(())
}
