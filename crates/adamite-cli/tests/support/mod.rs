//! Shared process invocation and JSON report assertions for CLI integration tests.

use serde_json::Value;
use std::{
    error::Error,
    path::Path,
    process::{Command, Output},
};

pub type TestResult = Result<(), Box<dyn Error>>;

pub fn command(name: &str, source: &Path, flags: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_adamite"))
        .arg(name)
        .arg(source)
        .args(flags)
        .output()
        .expect("run adamite")
}

pub fn json_with(name: &str, source: &Path, flags: &[&str]) -> Value {
    let mut flags = flags.to_vec();
    flags.push("--json");
    let output = command(name, source, &flags);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("JSON report")
}

pub fn save(source: &Path, destination: &Path, flags: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_adamite"))
        .arg("scan")
        .arg(source)
        .arg("-o")
        .arg(destination)
        .args(flags)
        .output()
        .expect("save snapshot with adamite")
}
