//! MCP wire behavior over an in-process duplex transport.

use adamite_core::scan::{ScanContext, ScanOutput};
use adamite_fixtures::{
    Fixture, VolumeImage,
    builders::{AttrBuilder, BootSectorBuilder, FileNameBuilder, RecordBuilder},
};
use adamite_io::{driver::SyncDriver, limits::ScanLimits, reader::SliceReader};
use adamite_mcp::{McpServer, ServerOptions, SnapshotOptions};
use rmcp::{
    RoleClient, ServiceExt,
    model::{CallToolRequestParams, CallToolResult},
    service::{QuitReason, RunningService, ServiceError},
};
use serde_json::{Value, json};
use std::{error::Error, fs, io::Cursor, path::Path, time::Duration};
use tokio::task::JoinHandle;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

struct Session {
    directory: tempfile::TempDir,
    output: ScanOutput,
    client: RunningService<RoleClient, ()>,
    server: JoinHandle<Result<QuitReason, tokio::task::JoinError>>,
}

impl Session {
    async fn new() -> TestResult<Self> {
        Self::with_scan_time(Some(116_444_736_000_000_000)).await
    }

    async fn with_scan_time(scanned_at_filetime: Option<u64>) -> TestResult<Self> {
        let directory = tempfile::tempdir()?;
        let fixture = Fixture::load(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/ntfs/basic-4k.json"),
        )?;
        let output = SyncDriver::with_limits(
            ScanLimits::default(),
            ScanContext {
                scanned_at_filetime,
                label: Some("mcp fixture".into()),
                ..ScanContext::default()
            },
        )?
        .run(&mut SliceReader::new(fixture.image()))?;
        fs::write(directory.path().join("C.adm"), output.to_bytes()?)?;
        let handler = McpServer::new(ServerOptions {
            snapshots: SnapshotOptions {
                snapshot_dir: Some(directory.path().to_owned()),
                ..SnapshotOptions::default()
            },
            stale_after: Duration::from_secs(1),
        })?;
        let (server_io, client_io) = tokio::io::duplex(64 * 1024);
        let server = tokio::spawn(async move {
            handler
                .serve(server_io)
                .await
                .expect("server handshake")
                .waiting()
                .await
        });
        let client = ().serve(client_io).await?;
        Ok(Self {
            directory,
            output,
            client,
            server,
        })
    }

    async fn call(&self, name: &str, args: Value) -> Result<CallToolResult, ServiceError> {
        self.client
            .call_tool(
                CallToolRequestParams::new(name.to_owned())
                    .with_arguments(args.as_object().expect("object arguments").clone()),
            )
            .await
    }

    async fn close(mut self) -> TestResult {
        self.client.close().await?;
        assert!(matches!(self.server.await??, QuitReason::Closed));
        Ok(())
    }
}

fn text(result: &CallToolResult) -> &str {
    assert_eq!(result.content.len(), 1);
    &result
        .content
        .first()
        .expect("one text block")
        .as_text()
        .expect("text content")
        .text
}

fn structured(result: &CallToolResult) -> TestResult<Value> {
    let value: Value = serde_json::from_str(text(result))?;
    assert_eq!(Some(&value), result.structured_content.as_ref());
    assert_eq!(value["schema"], "adamite/1");
    Ok(value)
}

fn tool_error(result: &CallToolResult, format: &str, code: &str) -> TestResult<String> {
    assert_eq!(result.is_error, Some(true));
    let message = if format == "json" {
        structured(result)?["error"]
            .as_str()
            .ok_or("error string")?
            .to_owned()
    } else {
        text(result).to_owned()
    };
    assert_eq!(message.lines().count(), 2, "{message}");
    assert!(
        message.starts_with(&format!("error[{code}]: ")),
        "{message}"
    );
    assert!(
        message
            .lines()
            .nth(1)
            .is_some_and(|line| line.starts_with("next: ")),
        "{message}"
    );
    Ok(message)
}

#[tokio::test(flavor = "current_thread")]
async fn handshake_tools_and_all_three_tools_in_both_formats() -> TestResult {
    let session = Session::new().await?;
    let info = session.client.peer_info().ok_or("server info")?;
    let identity = info.server_info.as_ref().ok_or("server identity")?;
    assert_eq!(identity.name, "adamite");
    assert_eq!(identity.version, env!("CARGO_PKG_VERSION"));
    let instructions = info.instructions.as_deref().ok_or("instructions")?;
    assert!(instructions.chars().count() <= 1000);
    for word in [
        "snapshots",
        "scan_volume",
        "UAC",
        "scan_status",
        "get_tree",
        "list_dir",
        "top_files",
        "search",
        "node_info",
        "Never delete",
        "Never retry",
    ] {
        assert!(instructions.contains(word), "missing {word}");
    }
    let tools = session.client.list_all_tools().await?;
    let mut names: Vec<_> = tools.iter().map(|tool| tool.name.as_ref()).collect();
    names.sort_unstable();
    assert_eq!(names, ["list_dir", "snapshot_info", "snapshots"]);
    for tool in tools {
        let annotations = tool.annotations.ok_or("annotations")?;
        assert_eq!(annotations.read_only_hint, Some(true));
        assert_eq!(annotations.idempotent_hint, Some(true));
        assert_eq!(annotations.open_world_hint, Some(false));
        assert!(tool.output_schema.is_none());
        assert!(tool.input_schema["properties"].get("snapshot").is_some());
        assert!(tool.input_schema["properties"].get("format").is_some());
    }
    let catalog = session.call("snapshots", json!({"format":"json"})).await?;
    let catalog = structured(&catalog)?;
    assert_eq!(catalog["items"][0]["name"], "C.adm");
    assert!(catalog["items"][0]["age_seconds"].is_null());
    assert!(catalog["items"][0]["stale"].is_null());
    let plain = session.call("snapshot_info", json!({})).await?;
    assert!(!plain.is_error.unwrap_or(false));
    assert!(text(&plain).starts_with(&format!(
        "{} · serial ",
        session.directory.path().join("C.adm").display()
    )));
    assert!(text(&plain).contains("scanned 1970-01-01T00:00:00Z ("));
    assert!(text(&plain).contains("sizes: on-disk allocation\n"));
    assert!(text(&plain).contains("mcp fixture"));
    let info = session
        .call("snapshot_info", json!({"snapshot":"c:", "format":"json"}))
        .await?;
    let info = structured(&info)?;
    assert_eq!(info["command"], "snapshot_info");
    assert_eq!(
        info["snapshot"]["path"],
        session
            .directory
            .path()
            .join("C.adm")
            .to_string_lossy()
            .as_ref()
    );
    assert_eq!(info["snapshot"]["stale"], true);
    assert!(info["snapshot"]["age_seconds"].as_u64().is_some());
    assert_eq!(info["tree"]["nodes"], session.output.snapshot.len());
    let listing = session.call("list_dir", json!({"format":"json"})).await?;
    let listing = structured(&listing)?;
    assert_eq!(listing["path"], "/");
    assert_eq!(listing["limit"], 50);
    assert_eq!(listing["offset"], 0);
    let plain = session
        .call("list_dir", json!({"path":"\\bigdir", "limit":2}))
        .await?;
    assert!(text(&plain).contains("f000"));
    assert!(text(&plain).contains("showing 1–2 of 200 · next offset=2"));
    let catalog = session
        .call("snapshots", json!({"snapshot":"C", "format":"json"}))
        .await?;
    let catalog = structured(&catalog)?;
    assert_eq!(catalog["items"][0]["scanned_at"], "1970-01-01T00:00:00Z");
    assert_eq!(catalog["items"][0]["stale"], true);
    let plain = session.call("snapshots", json!({})).await?;
    assert!(text(&plain).contains("C.adm"));
    assert!(text(&plain).contains("scanned 1970-01-01T00:00:00Z"));
    session.close().await
}

#[tokio::test(flavor = "current_thread")]
async fn directory_pages_end_at_null_and_past_end_is_empty() -> TestResult {
    let session = Session::new().await?;
    let mut offset = 0;
    let mut names = Vec::new();
    loop {
        let page = session
            .call(
                "list_dir",
                json!({"path":"/bigdir", "format":"json", "offset":offset, "limit":73}),
            )
            .await?;
        let page = structured(&page)?;
        assert_eq!(page["truncated"], false);
        assert_eq!(page["total_children"], 200);
        names.extend(
            page["items"]
                .as_array()
                .ok_or("items")?
                .iter()
                .map(|item| item["name"].as_str().expect("name").to_owned()),
        );
        match page["next_offset"].as_u64() {
            Some(next) => {
                assert!(next > offset);
                offset = next;
            }
            None => break,
        }
    }
    assert_eq!(names.len(), 200);
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), 200);
    for offset in [200, 1000] {
        let result = session
            .call(
                "list_dir",
                json!({"path":"/bigdir", "format":"json", "offset":offset}),
            )
            .await?;
        let page = structured(&result)?;
        assert_eq!(page["items"], json!([]));
        assert!(page["next_offset"].is_null());
        let result = session
            .call("list_dir", json!({"path":"/bigdir", "offset":offset}))
            .await?;
        assert!(!text(&result).contains("next offset="));
    }
    let last = session
        .call("list_dir", json!({"path":"/bigdir", "offset":199}))
        .await?;
    assert!(text(&last).contains("f199"));
    assert!(!text(&last).contains("next offset="));
    session.close().await
}

#[tokio::test(flavor = "current_thread")]
async fn tool_errors_are_actionable_and_failed_calls_leave_the_server_running() -> TestResult {
    let session = Session::new().await?;
    for format in ["text", "json"] {
        for limit in [-1, 0, 501] {
            let result = session
                .call("list_dir", json!({"format":format, "limit":limit}))
                .await?;
            assert!(tool_error(&result, format, "invalid_argument")?.contains("1 through 500"));
        }
        let result = session
            .call(
                "list_dir",
                json!({"format":format, "path":"/BIGDIR/missing/deeper"}),
            )
            .await?;
        assert!(
            tool_error(&result, format, "path_not_found")?
                .contains("list_dir with path \"/bigdir\"")
        );
        let result = session
            .call("list_dir", json!({"format":format, "path":"/bigdir/f000"}))
            .await?;
        assert!(tool_error(&result, format, "not_a_directory")?.contains("node_info"));
        let result = session
            .call("snapshot_info", json!({"format":format, "snapshot":"D:"}))
            .await?;
        assert!(tool_error(&result, format, "no_snapshot")?.contains("scan_volume"));
        fs::write(
            session.directory.path().join("D.adm"),
            session.output.to_bytes()?,
        )?;
        let result = session
            .call("snapshot_info", json!({"format":format}))
            .await?;
        assert!(tool_error(&result, format, "ambiguous_snapshot")?.contains("D.adm"));
        fs::remove_file(session.directory.path().join("D.adm"))?;
    }
    for (name, args) in [
        ("unknown", json!({})),
        ("list_dir", json!({"limit":"wrong type"})),
        ("snapshot_info", json!({"format":"yaml"})),
    ] {
        assert!(matches!(
            session.call(name, args).await,
            Err(ServiceError::McpError(_))
        ));
    }
    assert!(
        !session
            .call("snapshot_info", json!({}))
            .await?
            .is_error
            .unwrap_or(false)
    );
    fs::remove_file(session.directory.path().join("C.adm"))?;
    for format in ["text", "json"] {
        tool_error(
            &session
                .call("snapshot_info", json!({"format":format}))
                .await?,
            format,
            "no_snapshot",
        )?;
        let catalog = session.call("snapshots", json!({"format":format})).await?;
        assert!(!catalog.is_error.unwrap_or(false));
    }
    session.close().await
}

#[tokio::test(flavor = "current_thread")]
async fn unknown_and_future_scan_ages_are_null_in_queries() -> TestResult {
    for scanned_at in [None, Some(u64::MAX), Some(200_000_000_000_000_000)] {
        let session = Session::with_scan_time(scanned_at).await?;
        let result = session
            .call("snapshot_info", json!({"format":"json"}))
            .await?;
        let info = structured(&result)?;
        assert!(info["snapshot"]["age_seconds"].is_null());
        assert!(info["snapshot"]["stale"].is_null());
        let result = session.call("list_dir", json!({})).await?;
        assert!(text(&result).contains("(unknown) · sizes: on-disk allocation"));
        session.close().await?;
    }
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn ambiguous_paths_return_identifiers_that_can_be_used_for_retry() -> TestResult {
    let session = Session::new().await?;
    let fixture = Fixture::load(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/ntfs/basic-4k.json"),
    )?;
    // This fixture's MFT occupies one contiguous extent; replace two root files with duplicate directories.
    assert_eq!(fixture.manifest().mft.extents.len(), 1);
    let geometry = &fixture.manifest().geometry;
    let layout = VolumeImage::new(
        BootSectorBuilder {
            mft: geometry.mft_lcn,
            ..BootSectorBuilder::default()
        },
        fixture.manifest().image_len,
    )?;
    let mut image = Cursor::new(fixture.image().to_vec());
    for record_number in [271, 320] {
        let name = AttrBuilder::resident(
            0x30,
            FileNameBuilder {
                name: "same".encode_utf16().collect(),
                ..FileNameBuilder::default()
            }
            .build(),
        )
        .build();
        let record = RecordBuilder {
            record_number,
            sequence: 1,
            flags: 3,
            attributes: vec![name],
            ..RecordBuilder::default()
        };
        layout.write_record(&mut image, u64::from(record_number), &record.build())?;
    }
    let output = SyncDriver::with_limits(ScanLimits::default(), ScanContext::default())?
        .run(&mut SliceReader::new(image.get_ref()))?;
    fs::write(session.directory.path().join("C.adm"), output.to_bytes()?)?;
    for format in ["text", "json"] {
        let result = session
            .call("list_dir", json!({"path":"/same", "format":format}))
            .await?;
        let error = tool_error(&result, format, "ambiguous_path")?;
        let next = error.lines().nth(1).ok_or("next step")?;
        let candidates: Vec<_> = next
            .strip_prefix("next: Pass one of these paths: ")
            .ok_or("candidate paths")?
            .split(", ")
            .collect();
        assert_eq!(candidates.len(), 2);
        for path in candidates {
            assert!(path.starts_with("/#"), "{path}");
            let result = session
                .call("list_dir", json!({"path":path, "format":"json"}))
                .await?;
            assert!(!result.is_error.unwrap_or(false));
            assert_eq!(structured(&result)?["items"], json!([]));
            let missing = session
                .call(
                    "list_dir",
                    json!({"path":format!("{path}/missing"), "format":format}),
                )
                .await?;
            let error = tool_error(&missing, format, "path_not_found")?;
            assert!(
                error.contains(&format!("list_dir with path {path:?}")),
                "{error}"
            );
        }
    }
    session.close().await
}
