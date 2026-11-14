//! Length and scan context supplied by an image reader.

use adamite_io::reader::{ImageReader, VolumeReader};
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

#[test]
fn length_matches_file_size_and_bounds_reads() -> Result<(), Box<dyn Error>> {
    let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join("image-reader-length.img");
    let bytes: Vec<u8> = (0..=u8::MAX).cycle().take(4096 + 7).collect();
    fs::write(&path, &bytes)?;
    let mut reader = ImageReader::open(&path)?;

    assert_eq!(reader.len_bytes(), 4103);
    let mut tail = [0; 7];
    reader.read_exact_at(4096, &mut tail)?;
    assert_eq!(tail.as_slice(), bytes.get(4096..).ok_or("tail missing")?);
    assert!(reader.read_exact_at(4097, &mut tail).is_err());
    Ok(())
}

/// Opens the block device named by `ADAMITE_BLOCK_DEVICE`, such as `/dev/loop0`, which requires read access to it.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires ADAMITE_BLOCK_DEVICE and read access to that device"]
fn block_device_length_matches_sysfs_size() -> Result<(), Box<dyn Error>> {
    let device = std::env::var_os("ADAMITE_BLOCK_DEVICE").ok_or("ADAMITE_BLOCK_DEVICE unset")?;
    let device = fs::canonicalize(device)?;
    let name = device.file_name().ok_or("device path has no name")?;
    // sysfs reports the size in 512-byte sectors regardless of the logical sector size.
    let sectors: u64 = fs::read_to_string(Path::new("/sys/class/block").join(name).join("size"))?
        .trim()
        .parse()?;
    let expected = sectors.checked_mul(512).ok_or("device size overflow")?;

    let mut reader = ImageReader::open(&device)?;

    assert!(expected > 0, "device {} is empty", device.display());
    assert_eq!(reader.len_bytes(), expected);
    let mut boot = [0; 512];
    reader.read_exact_at(0, &mut boot)?;
    Ok(())
}
