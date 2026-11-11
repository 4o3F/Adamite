//! Elevated Windows CLI scan, snapshot persistence and saved-report checks.

#![cfg(windows)]

mod support;

use std::{fs, io, path::Path};
use support::{TestResult, json_with, save};

#[test]
#[ignore = "requires an elevated Windows session"]
fn scan_drive_and_query_saved_snapshot() -> TestResult {
    let directory = Path::new(env!("CARGO_TARGET_TMPDIR"));
    fs::create_dir_all(directory)?;
    let path = directory.join("C.adm");
    match fs::remove_file(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    let output = save(Path::new("C:"), &path, &["--force"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let info = json_with("info", &path, &[]);
    let top = json_with("top", &path, &["--n", "5"]);
    for report in [&info, &top] {
        for field in ["scanned_at", "scanned_at_filetime"] {
            assert!(
                report["snapshot"][field]
                    .as_str()
                    .is_some_and(|value| !value.is_empty()),
                "missing {field}: {report}"
            );
        }
    }
    assert!(
        info["tree"]["nodes"]
            .as_u64()
            .is_some_and(|count| count > 0),
        "expected a positive node count: {info}"
    );
    let items = top["items"]
        .as_array()
        .ok_or("top items must be an array")?;
    assert!(
        !items.is_empty() && items.len() <= 5,
        "top items: {items:?}"
    );

    fs::remove_file(&path)?;
    Ok(())
}
