//! Drive validation and elevated Windows raw-volume scan checks.

#![cfg(windows)]

use adamite_core::scan::{ScanConfig, ScanOutput};
use adamite_io::{
    driver::SyncDriver,
    error::VolumeError,
    reader::{RawVolumeReader, VolumeReader},
};
use std::error::Error;

type TestResult = Result<(), Box<dyn Error>>;

#[test]
fn invalid_drive_letters_are_rejected() {
    for letter in ['1', 'é'] {
        assert!(
            matches!(
                RawVolumeReader::open_drive(letter),
                Err(VolumeError::InvalidDrive { letter: rejected }) if rejected == letter
            ),
            "drive letter {letter:?} should be rejected"
        );
    }
}

#[test]
#[ignore = "requires an elevated Windows session"]
fn drive_length_is_a_positive_sector_multiple() -> TestResult {
    let reader = RawVolumeReader::open_drive('C')?;
    assert!(reader.len_bytes() > 0);
    assert!(reader.len_bytes().is_multiple_of(512));
    Ok(())
}

#[test]
#[ignore = "requires an elevated Windows session"]
fn boot_sector_has_the_ntfs_oem_id() -> TestResult {
    let mut reader = RawVolumeReader::open_drive('C')?;
    let mut boot = [0u8; 512];
    reader.read_exact_at(0, &mut boot)?;
    assert_eq!(boot.get(3..11), Some(b"NTFS    ".as_slice()));
    Ok(())
}

#[test]
#[ignore = "requires an elevated Windows session"]
fn final_sector_is_readable() -> TestResult {
    let mut reader = RawVolumeReader::open_drive('C')?;
    let offset = reader
        .len_bytes()
        .checked_sub(512)
        .ok_or("volume is smaller than one sector")?;
    let mut sector = [0u8; 512];
    reader.read_exact_at(offset, &mut sector)?;
    Ok(())
}

#[test]
#[ignore = "requires an elevated Windows session"]
fn context_has_a_timestamp_and_total_free_bytes() -> TestResult {
    let reader = RawVolumeReader::open_drive('C')?;
    let context = reader.context()?;
    assert!(
        context
            .scanned_at_filetime
            .is_some_and(|ticks| ticks > 116_444_736_000_000_000)
    );
    assert!(context.free_bytes.is_some());
    Ok(())
}

#[test]
#[ignore = "requires an elevated Windows session"]
fn full_scan_produces_a_nonempty_snapshot() -> TestResult {
    let mut reader = RawVolumeReader::open_drive('C')?;
    let driver = SyncDriver::new(ScanConfig {
        context: reader.context()?,
        ..Default::default()
    })?;
    let output = driver.run(&mut reader)?;
    assert!(!output.snapshot.is_empty());
    assert!(output.stats.records_in_use > 0);

    let bytes = output.to_bytes()?;
    let decoded = ScanOutput::from_bytes(&bytes)?;
    assert_eq!(decoded, output, "real-volume scan output must round-trip");
    assert_eq!(
        decoded.to_bytes()?,
        bytes,
        "re-encoding the decoded output must preserve every byte"
    );
    Ok(())
}
