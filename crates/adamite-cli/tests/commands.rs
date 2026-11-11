//! Process-level coverage of source detection, reports and command failures.

use adamite_core::{
    scan::{ScanConfig, ScanContext, ScanOutput, ScanStats},
    snapshot::NodeId,
};
use adamite_fixtures::Fixture;
use adamite_io::{driver::SyncDriver, reader::SliceReader};
use serde_json::{Value, json};
use std::{
    error::Error,
    fs,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

type TestResult = Result<(), Box<dyn Error>>;

struct Case {
    directory: PathBuf,
    image: PathBuf,
    output: ScanOutput,
}

impl Case {
    fn new(name: &str, fixture_name: &str) -> Result<Self, Box<dyn Error>> {
        let directory = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
        fs::create_dir_all(&directory)?;
        let fixture = Fixture::load(
            &Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(format!("../../tests/fixtures/ntfs/{fixture_name}.json")),
        )?;
        let image = directory.join("source.img");
        fs::write(&image, fixture.image())?;
        let mut reader = SliceReader::new(fixture.image());
        let output = SyncDriver::new(ScanConfig::default())?.run(&mut reader)?;
        Ok(Self {
            directory,
            image,
            output,
        })
    }

    fn command(&self, name: &str, source: &Path, flags: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_adamite"))
            .arg(name)
            .arg(source)
            .args(flags)
            .output()
            .expect("run adamite")
    }

    fn json(&self, name: &str, source: &Path) -> Value {
        let output = self.command(name, source, &["--json"]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).expect("JSON report")
    }

    fn snapshot(&self, name: &str) -> Result<PathBuf, Box<dyn Error>> {
        let path = self.directory.join(name);
        fs::write(&path, self.output.to_bytes()?)?;
        Ok(path)
    }

    fn assert_failure(&self, command: &str, path: &Path, code: i32) -> Output {
        let output = self.command(command, path, &["--json"]);
        assert_eq!(
            output.status.code(),
            Some(code),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stdout.is_empty(), "error wrote to stdout");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.starts_with("error: "), "{stderr}");
        assert!(!stderr.contains("panicked"), "{stderr}");
        output
    }
}

impl Drop for Case {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn expected_stats(stats: &ScanStats) -> Value {
    json!({
        "records_total": stats.records_total,
        "records_in_use": stats.records_in_use,
        "base_records": stats.base_records,
        "extension_records": stats.extension_records,
        "unused": stats.unused,
        "deleted": stats.deleted,
        "baad": stats.baad,
        "corrupt_records": stats.corrupt_records,
        "bad_fixup": stats.bad_fixup,
        "invalid_fixup_metadata": stats.invalid_fixup_metadata,
        "invalid_signature": stats.invalid_signature,
        "corrupt_headers": stats.corrupt_headers,
        "corrupt_attributes": stats.corrupt_attributes,
        "type_storage_mismatches": stats.type_storage_mismatches,
        "corrupt_values": stats.corrupt_values,
        "corrupt_runlists": stats.corrupt_runlists,
        "out_of_volume_extents": stats.out_of_volume_extents,
        "invalid_attribute_lists": stats.invalid_attribute_lists,
        "invalid_file_refs": stats.invalid_file_refs,
        "invalid_utf16": stats.invalid_utf16,
        "rejected_extensions": stats.rejected_extensions,
        "corrupt_streams": stats.corrupt_streams,
        "orphans": stats.orphans,
        "cycle_nodes": stats.cycle_nodes,
        "hardlink_duplicates": stats.hardlink_duplicates,
        "metafiles": stats.metafiles,
    })
}

fn without_source_and_time(mut value: Value) -> Value {
    let snapshot = value["snapshot"].as_object_mut().expect("snapshot object");
    for field in ["source", "scanned_at", "scanned_at_filetime"] {
        snapshot.remove(field);
    }
    value
}

#[test]
fn info_json_matches_in_process_scans() -> TestResult {
    for fixture_name in ["basic-4k", "frag-512"] {
        let case = Case::new(&format!("json-{fixture_name}"), fixture_name)?;
        let output = case.command("info", &case.image, &["--json", "--bytes"]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8(output.stderr)?,
            format!("scanning {}…\n", case.image.display())
        );
        let value: Value = serde_json::from_slice(&output.stdout)?;
        let snapshot = &case.output.snapshot;
        let root = snapshot.node(NodeId::ROOT).ok_or("missing root")?;
        let geometry = snapshot.volume().geometry();
        assert_eq!(value["schema"], "adamite/1");
        assert_eq!(value["command"], "info");
        assert_eq!(value["units"], json!({"size": "bytes"}));
        assert_eq!(value["snapshot"].as_object().ok_or("snapshot")?.len(), 5);
        assert_eq!(
            value["snapshot"]["source"],
            case.image.to_string_lossy().as_ref()
        );
        assert_eq!(
            value["snapshot"]["volume_serial"],
            format!("{:016x}", geometry.serial())
        );
        assert_eq!(value["snapshot"]["label"], Value::Null);
        let time = value["snapshot"]["scanned_at"]
            .as_str()
            .ok_or("scan time")?;
        assert_eq!(time.len(), 20, "UTC seconds precision: {time}");
        assert!(time.ends_with('Z'));
        let filetime = value["snapshot"]["scanned_at_filetime"]
            .as_str()
            .ok_or("raw scan time")?
            .parse::<u64>()?;
        assert!(filetime > 0);
        assert_eq!(
            value["volume"],
            json!({
                "bytes_per_sector": geometry.bytes_per_sector(), "cluster_size": geometry.cluster_size(),
                "total_bytes": geometry.total_bytes(), "free_bytes": null,
            })
        );
        assert_eq!(
            value["tree"],
            json!({
                "nodes": snapshot.len(), "files": root.file_count(),
                "total_size": root.total_size(), "total_alloc": root.total_alloc(),
            })
        );
        assert_eq!(value["stats"], expected_stats(&case.output.stats));
        assert_eq!(value.as_object().ok_or("report")?.len(), 7);
    }
    Ok(())
}

#[test]
fn snapshot_extension_and_magic_match_image_report() -> TestResult {
    let case = Case::new("snapshot-equivalence", "basic-4k")?;
    let image = case.json("info", &case.image);
    for name in ["saved.adm", "saved.ADM", "saved.bin"] {
        let path = case.snapshot(name)?;
        let output = case.command("info", &path, &["--json"]);
        assert!(output.status.success());
        assert!(output.stderr.is_empty(), "snapshot was scanned");
        let value: Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(value["snapshot"]["source"], path.to_string_lossy().as_ref());
        assert_eq!(value["snapshot"]["scanned_at"], Value::Null);
        assert_eq!(value["snapshot"]["scanned_at_filetime"], Value::Null);
        assert_eq!(
            without_source_and_time(value),
            without_source_and_time(image.clone())
        );
    }
    Ok(())
}

#[test]
fn snapshot_optional_metadata_and_time_are_preserved() -> TestResult {
    let mut case = Case::new("snapshot-metadata", "basic-4k")?;
    let bytes = fs::read(&case.image)?;
    for (raw, rendered) in [
        (0, Some("1601-01-01T00:00:00Z")),
        (116_444_736_012_345_678, Some("1970-01-01T00:00:01Z")),
        (u64::MAX, None),
    ] {
        let config = ScanConfig {
            context: ScanContext {
                scanned_at_filetime: Some(raw),
                label: Some("fixture\nlabel".into()),
                free_bytes: Some(1234),
            },
            ..ScanConfig::default()
        };
        case.output = SyncDriver::new(config)?.run(&mut SliceReader::new(&bytes))?;
        let path = case.snapshot("metadata.adm")?;
        let value = case.json("info", &path);
        assert_eq!(value["snapshot"]["scanned_at"], json!(rendered));
        assert_eq!(value["snapshot"]["scanned_at_filetime"], raw.to_string());
        assert_eq!(value["snapshot"]["label"], "fixture\nlabel");
        assert_eq!(value["volume"]["free_bytes"], 1234);
        let text = case.command("info", &path, &[]);
        assert!(String::from_utf8(text.stdout)?.contains("fixture\\u{a}label"));
    }
    Ok(())
}

#[test]
fn scan_prints_info_with_scan_command_name() -> TestResult {
    let case = Case::new("scan-report", "basic-4k")?;
    let mut scan = case.json("scan", &case.image);
    assert_eq!(scan["command"], "scan");
    scan["command"] = json!("info");
    assert_eq!(
        without_source_and_time(scan),
        without_source_and_time(case.json("info", &case.image))
    );
    Ok(())
}

#[test]
fn basic_info_text_has_stable_totals_and_nonzero_counters() -> TestResult {
    let case = Case::new("basic-text", "basic-4k")?;
    let expected = concat!(
        "  Volume serial            34f5ee1202469ff7\n",
        "  Bytes per sector         512 B\n",
        "  Cluster size             4.0 KiB\n",
        "  Volume size              16.0 MiB\n",
        "  Nodes                    280\n",
        "  Files                    231\n",
        "  Logical size             3.8 MiB\n",
        "  Allocated size           18.9 MiB\n",
        "  records_total            323\n",
        "  records_in_use           277\n",
        "  base_records             277\n",
        "  deleted                  46\n",
        "  hardlink_duplicates      2\n",
        "  metafiles                18\n",
    );
    let output = case.command("info", &case.image, &[]);
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout)?;
    let (header, body) = text.split_once('\n').ok_or("missing header")?;
    assert!(header.starts_with(&format!("{} — scanned ", case.image.display())));
    assert_eq!(body, expected);

    let expected_bytes = concat!(
        "  Volume serial            34f5ee1202469ff7\n",
        "  Bytes per sector         512\n",
        "  Cluster size             4096\n",
        "  Volume size              16776704\n",
        "  Nodes                    280\n",
        "  Files                    231\n",
        "  Logical size             3950798\n",
        "  Allocated size           19812352\n",
        "  records_total            323\n",
        "  records_in_use           277\n",
        "  base_records             277\n",
        "  deleted                  46\n",
        "  hardlink_duplicates      2\n",
        "  metafiles                18\n",
    );
    let output = case.command("info", &case.image, &["--bytes"]);
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout)?;
    assert_eq!(
        text.split_once('\n').ok_or("missing header")?.1,
        expected_bytes
    );
    Ok(())
}

#[test]
fn damaged_snapshots_never_fall_back_to_scanning() -> TestResult {
    let case = Case::new("corrupt-snapshots", "basic-4k")?;
    let bytes = case.output.to_bytes()?;
    let mut damaged = Vec::new();
    for length in 0..8 {
        damaged.push(bytes[..length].to_vec());
    }
    for offset in 0..8 {
        let mut changed = bytes.clone();
        changed[offset] ^= 1;
        damaged.push(changed);
    }
    let mut changed = bytes.clone();
    *changed.last_mut().ok_or("empty snapshot")? ^= 1;
    damaged.push(changed);
    for (index, bytes) in damaged.iter().enumerate() {
        let path = case.directory.join(format!("damaged-{index}.adm"));
        fs::write(&path, bytes)?;
        let output = case.assert_failure("info", &path, 3);
        let stderr = String::from_utf8(output.stderr)?;
        assert!(stderr.contains("caused by: "), "{stderr}");
        assert!(!stderr.contains("scanning "), "{stderr}");
    }
    Ok(())
}

#[test]
fn missing_source_and_snapshot_scan_fail_without_stdout() -> TestResult {
    let case = Case::new("source-failures", "basic-4k")?;
    let missing = case.directory.join("missing.img");
    let output = case.assert_failure("info", &missing, 1);
    assert!(String::from_utf8(output.stderr)?.contains("caused by: "));
    let path = case.snapshot("saved.adm")?;
    let output = case.assert_failure("scan", &path, 2);
    assert!(String::from_utf8(output.stderr)?.contains("already a snapshot"));
    let magic = case.snapshot("saved.bin")?;
    case.assert_failure("scan", &magic, 2);
    Ok(())
}

#[cfg(not(windows))]
#[test]
fn drive_sources_require_windows() -> TestResult {
    let case = Case::new("drive-failure", "basic-4k")?;
    for source in ["C:", "c:"] {
        let output = case.assert_failure("scan", Path::new(source), 2);
        assert!(String::from_utf8(output.stderr)?.contains("raw volumes require Windows"));
    }
    Ok(())
}

#[test]
fn clap_usage_help_and_version() -> TestResult {
    for args in [
        vec!["info", "unused", "--invalid"],
        vec!["scan", "unused", "-o", "out.adm"],
        vec!["scan", "unused", "--force"],
        vec!["info"],
        vec!["--json"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_adamite"))
            .args(&args)
            .output()?;
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8(output.stderr)?.starts_with("error:"));
    }
    for args in [
        vec![],
        vec!["--help"],
        vec!["info", "--help"],
        vec!["--version"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_adamite"))
            .args(&args)
            .output()?;
        assert!(output.status.success(), "{args:?}");
        assert!(output.stderr.is_empty());
        let text = String::from_utf8(output.stdout)?;
        assert!(
            text.contains(if args.contains(&"--version") {
                "adamite 0.1.0"
            } else {
                "Usage:"
            }),
            "{text}"
        );
    }
    Ok(())
}

#[test]
fn broken_stdout_pipe_is_success() -> TestResult {
    let case = Case::new("broken-pipe", "basic-4k")?;
    let mut child = Command::new(env!("CARGO_BIN_EXE_adamite"))
        .arg("info")
        .arg(&case.image)
        .arg("--json")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    drop(child.stdout.take());
    let output = child.wait_with_output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!String::from_utf8(output.stderr)?.contains("error:"));
    Ok(())
}

#[cfg(unix)]
#[test]
fn unreadable_source_exits_four_when_permissions_are_enforced() -> TestResult {
    use std::io;
    use std::os::unix::fs::PermissionsExt;
    let case = Case::new("permission-failure", "basic-4k")?;
    let permissions = fs::metadata(&case.image)?.permissions();
    fs::set_permissions(&case.image, fs::Permissions::from_mode(0o0))?;
    let unreadable = matches!(fs::File::open(&case.image), Err(error) if error.kind() == io::ErrorKind::PermissionDenied);
    if unreadable {
        case.assert_failure("info", &case.image, 4);
    }
    fs::set_permissions(&case.image, permissions)?;
    Ok(())
}
