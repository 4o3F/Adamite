//! Scan context supplied by an image reader.

use adamite_io::reader::ImageReader;
use nt_time::FileTime;
use std::{error::Error, fs, path::Path, time::SystemTime};

#[test]
fn context_has_current_timestamp_without_label_or_free_bytes() -> Result<(), Box<dyn Error>> {
    // FILETIME counts 100-nanosecond ticks since 1601-01-01.
    const UNIX_EPOCH_FILETIME: u64 = 116_444_736_000_000_000;

    let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join("image-reader-context.img");
    fs::write(&path, [])?;
    let reader = ImageReader::open(&path)?;

    let context = reader.context()?;
    let after = FileTime::try_from(SystemTime::now())?.to_raw();
    let timestamp = context
        .scanned_at_filetime
        .ok_or("scan timestamp missing")?;

    assert!(
        timestamp > UNIX_EPOCH_FILETIME,
        "scan timestamp {timestamp} must be later than the Unix epoch"
    );
    assert!(
        timestamp <= after,
        "scan timestamp {timestamp} must not exceed the subsequent FILETIME {after}"
    );
    assert_eq!(context.label, None);
    assert_eq!(context.free_bytes, None);
    Ok(())
}
