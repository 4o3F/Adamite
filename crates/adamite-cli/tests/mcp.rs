//! MCP subprocess framing, query behavior and clean EOF shutdown on every host platform.

use adamite_core::scan::ScanConfig;
use adamite_fixtures::Fixture;
use adamite_io::{driver::SyncDriver, reader::SliceReader};
use serde_json::{Value, json};
use std::{
    error::Error,
    fs,
    io::{BufRead, BufReader, Write},
    path::Path,
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

struct Process(Child);

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn stdio_handshake_tree_protocol_only_stdout_and_successful_eof() -> Result<(), Box<dyn Error>> {
    let directory = tempfile::tempdir()?;
    let fixture = Fixture::load(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/ntfs/basic-4k.json"),
    )?;
    let output =
        SyncDriver::new(ScanConfig::default())?.run(&mut SliceReader::new(fixture.image()))?;
    let snapshot = directory.path().join("fixture.adm");
    fs::write(&snapshot, output.to_bytes()?)?;
    let mut process = Process(
        Command::new(env!("CARGO_BIN_EXE_adamite"))
            .args(["mcp", "--snapshot"])
            .arg(&snapshot)
            .arg("--snapshot-dir")
            .arg(directory.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?,
    );
    let mut stdin = process.0.stdin.take().ok_or("child stdin")?;
    let stdout = process.0.stdout.take().ok_or("child stdout")?;
    let (sender, receiver) = mpsc::channel();
    let reader = thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let message = line.map_err(|error| error.to_string()).and_then(|line| {
                serde_json::from_str::<Value>(&line).map_err(|error| error.to_string())
            });
            if sender.send(message).is_err() {
                break;
            }
        }
    });
    writeln!(
        stdin,
        "{}",
        json!({
            "jsonrpc":"2.0", "id":1, "method":"initialize",
            "params":{"protocolVersion":"2025-03-26", "capabilities":{}, "clientInfo":{"name":"stdio-test", "version":"1"}}
        })
    )?;
    stdin.flush()?;
    let receive = || -> Result<Value, Box<dyn Error>> {
        let message = receiver.recv_timeout(Duration::from_secs(15))??;
        assert_eq!(message["jsonrpc"], "2.0");
        assert!(message.get("method").is_some() || message.get("id").is_some());
        Ok(message)
    };
    let handshake = receive()?;
    assert_eq!(handshake["id"], 1);
    assert_eq!(handshake["result"]["serverInfo"]["name"], "adamite");
    writeln!(
        stdin,
        "{}",
        json!({"jsonrpc":"2.0", "method":"notifications/initialized"})
    )?;
    writeln!(
        stdin,
        "{}",
        json!({
            "jsonrpc":"2.0", "id":2, "method":"tools/call",
            "params":{"name":"get_tree", "arguments":{"snapshot":snapshot, "budget":12, "format":"json"}}
        })
    )?;
    stdin.flush()?;
    let tree = receive()?;
    assert_eq!(tree["id"], 2);
    assert_eq!(tree["result"]["isError"], false, "{tree}");
    let content = tree["result"]["content"][0]["text"]
        .as_str()
        .ok_or("tree content")?;
    let report: Value = serde_json::from_str(content)?;
    assert_eq!(report["command"], "get_tree");
    assert!(!report["rows"].as_array().ok_or("tree rows")?.is_empty());
    assert_eq!(tree["result"]["structuredContent"], report);
    drop(stdin);
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        if let Some(status) = process.0.try_wait()? {
            break status;
        }
        assert!(Instant::now() < deadline, "MCP did not exit after EOF");
        thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(status.code(), Some(0));
    reader.join().map_err(|_| "stdout reader panicked")?;
    for message in receiver.try_iter() {
        let message = message?;
        assert_eq!(message["jsonrpc"], "2.0");
        assert!(message.get("method").is_some() || message.get("id").is_some());
    }
    Ok(())
}
