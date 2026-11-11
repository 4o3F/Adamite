//! Portable argument quoting and Windows token queries.

use adamite_io::elevation::{CommandLine, ElevationError};
use std::{error::Error, ffi::OsStr};

#[test]
fn quotes_arguments_with_windows_backslash_rules() -> Result<(), Box<dyn Error>> {
    for (args, expected) in [
        (vec![], ""),
        (vec!["a b", ""], "\"a b\" \"\""),
        (
            vec![r"C:\path\", r"two\\"],
            "\"C:\\path\\\\\" \"two\\\\\\\\\"",
        ),
        (vec![r"C:\inside\path"], "\"C:\\inside\\path\""),
        (vec!["数据", "é 😀"], "\"数据\" \"é 😀\""),
    ] {
        let parameters = CommandLine::new(&args)?;
        assert_eq!(parameters.as_os_str(), OsStr::new(expected), "{args:?}");
    }
    Ok(())
}

#[test]
fn rejects_quotes_and_nuls() {
    for arg in ["bad\"quote", "bad\0nul"] {
        assert!(matches!(
            CommandLine::new([arg]),
            Err(ElevationError::Argument { argument, .. }) if argument == arg
        ));
    }
}

#[cfg(windows)]
#[test]
fn queries_current_process_elevation() -> Result<(), Box<dyn Error>> {
    adamite_io::elevation::ElevatedProcess::is_current()?;
    Ok(())
}
