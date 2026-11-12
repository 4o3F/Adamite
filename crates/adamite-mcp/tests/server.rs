//! MCP wire behavior over an in-process duplex transport.

use adamite_core::scan::{ScanContext, ScanOutput};
use adamite_fixtures::{
    Fixture, SyntheticConfig, SyntheticVolume, VolumeImage,
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
async fn handshake_lists_ten_annotated_tools_and_baseline_reports() -> TestResult {
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
    assert_eq!(
        names,
        [
            "ext_breakdown",
            "get_tree",
            "list_dir",
            "node_info",
            "scan_status",
            "scan_volume",
            "search",
            "snapshot_info",
            "snapshots",
            "top_files"
        ]
    );
    for tool in tools {
        let annotations = tool.annotations.ok_or("annotations")?;
        let is_scan = tool.name == "scan_volume";
        assert_eq!(annotations.read_only_hint, Some(!is_scan));
        assert_eq!(annotations.idempotent_hint, Some(!is_scan));
        if is_scan {
            assert_eq!(annotations.destructive_hint, Some(false));
        }
        assert_eq!(annotations.open_world_hint, Some(false));
        assert!(tool.output_schema.is_none());
        let selector = if tool.name.starts_with("scan_") {
            "volume"
        } else {
            "snapshot"
        };
        assert!(tool.input_schema["properties"].get(selector).is_some());
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

#[tokio::test(flavor = "current_thread")]
async fn remaining_queries_match_access_reports_in_text_and_json() -> TestResult {
    use adamite_access::{
        BreakdownBy, BreakdownStats,
        report::{ExtReport, KindReport, NodeReport, TopReport, TreeReport},
    };
    use adamite_core::snapshot::{FileFilter, FileKind};

    let session = Session::new().await?;
    let path = session.directory.path().join("C.adm");
    let output = &session.output;
    let mut cases = vec![
        (
            "get_tree",
            json!({}),
            serde_json::to_value(TreeReport::new(&path, output, "/", 60, 50)?)?,
        ),
        (
            "top_files",
            json!({}),
            serde_json::to_value(TopReport::new(&path, output, 20)?)?,
        ),
        (
            "top_files",
            json!({"under":"/bigdir", "ext":["", ".TXT"]}),
            serde_json::to_value(TopReport::select(
                &path,
                output,
                20,
                Some("/bigdir"),
                Some(&FileFilter::extensions(["", ".TXT"])?),
            )?)?,
        ),
        (
            "top_files",
            json!({"kind":"document"}),
            serde_json::to_value(TopReport::select(
                &path,
                output,
                20,
                None,
                Some(&FileFilter::Kind(FileKind::Document)),
            )?)?,
        ),
        (
            "node_info",
            json!({"path":"/bigdir/f000"}),
            serde_json::to_value(NodeReport::new(&path, output, "/bigdir/f000")?)?,
        ),
        (
            "node_info",
            json!({"path":"/bigdir"}),
            serde_json::to_value(NodeReport::new(&path, output, "/bigdir")?)?,
        ),
        (
            "ext_breakdown",
            json!({}),
            serde_json::to_value(ExtReport::new(&path, output, 20))?,
        ),
    ];
    for (by, grouping) in [
        ("extension", BreakdownBy::Extension),
        ("kind", BreakdownBy::Kind),
    ] {
        let stats = BreakdownStats::new(&output.snapshot, Some("/bigdir"), grouping)?;
        let expected = match stats {
            BreakdownStats::Extensions(stats) => {
                serde_json::to_value(ExtReport::from_stats(&path, output, &stats, 20))?
            }
            BreakdownStats::Kinds(stats) => {
                serde_json::to_value(KindReport::new(&path, output, &stats, 20))?
            }
        };
        cases.push((
            "ext_breakdown",
            json!({"under":"/bigdir", "by":by}),
            expected,
        ));
    }
    for (name, args, expected) in cases {
        let expected: Value = serde_json::from_str(&expected.to_string())?;
        for format in ["text", "json"] {
            let mut args = args.clone();
            args["format"] = json!(format);
            let result = session.call(name, args).await?;
            assert!(
                !result.is_error.unwrap_or(false),
                "{name}: {}",
                text(&result)
            );
            if format == "json" {
                let actual = structured(&result)?;
                assert_eq!(actual["command"], name);
                for key in [
                    "items",
                    "rows",
                    "path",
                    "name",
                    "node_id",
                    "flags",
                    "hard_link_count",
                    "mtime_filetime",
                    "modified_at",
                    "total_size",
                    "total_alloc",
                    "size",
                    "alloc",
                    "kind",
                ] {
                    assert_eq!(actual.get(key), expected.get(key), "{name} {key}");
                }
                assert!(actual["next_offset"].is_null());
            } else {
                assert!(text(&result).contains(" · serial "));
                assert!(text(&result).contains("sizes: on-disk allocation\n"));
                if name == "node_info" {
                    assert!(text(&result).contains("NodeId:"));
                    assert!(text(&result).contains("hard links:"));
                }
            }
        }
    }
    session.close().await
}

#[tokio::test(flavor = "current_thread")]
async fn search_paging_reaches_null_and_offsets_past_end_are_empty() -> TestResult {
    let session = Session::new().await?;
    let mut offset = 0;
    let mut paths = Vec::new();
    loop {
        let args = json!({"pattern":"f*", "under":"/bigdir", "offset":offset, "limit":67, "format":"json"});
        let result = session.call("search", args.clone()).await?;
        let page = structured(&result)?;
        assert_eq!(page["total_matches"], 200);
        let items = page["items"].as_array().ok_or("items")?;
        assert!(!items.is_empty());
        paths.extend(
            items
                .iter()
                .map(|item| item["path"].as_str().expect("path").to_owned()),
        );
        let mut args = args;
        args["format"] = json!("text");
        let plain = session.call("search", args).await?;
        if let Some(next) = page["next_offset"].as_u64() {
            assert!(next > offset);
            assert!(text(&plain).contains(&format!("next offset={next}")));
            offset = next;
        } else {
            assert!(!text(&plain).contains("next offset="));
            break;
        }
    }
    paths.sort_unstable();
    paths.dedup();
    assert_eq!(paths.len(), 200);
    for pattern in ["f*", "F0", "no-match-at-all"] {
        let result = session
            .call(
                "search",
                json!({"pattern":pattern,"under":"/bigdir","offset":500,"format":"json"}),
            )
            .await?;
        let page = structured(&result)?;
        assert_eq!(page["items"], json!([]));
        assert!(page["next_offset"].is_null());
    }
    session.close().await
}

#[tokio::test(flavor = "current_thread")]
async fn invalid_query_arguments_and_directory_scopes_have_actionable_errors() -> TestResult {
    let session = Session::new().await?;
    let cases = [
        ("get_tree", json!({"budget":0}), "1 through 400"),
        ("get_tree", json!({"budget":401}), "1 through 400"),
        (
            "get_tree",
            json!({"min_share_percent":-0.1}),
            "0 through 100",
        ),
        (
            "get_tree",
            json!({"min_share_percent":100.1}),
            "0 through 100",
        ),
        ("top_files", json!({"n":0}), "1 through 100"),
        ("top_files", json!({"n":101}), "1 through 100"),
        (
            "top_files",
            json!({"ext":[], "kind":"code"}),
            "either ext or kind",
        ),
        ("top_files", json!({"kind":"unknown"}), "disk-image"),
        ("ext_breakdown", json!({"n":201}), "1 through 200"),
        ("ext_breakdown", json!({"n":0}), "1 through 200"),
        ("search", json!({"pattern":"["}), "[abc]"),
        ("search", json!({"pattern":"a".repeat(257)}), "256 bytes"),
        ("search", json!({"pattern":"*", "limit":0}), "1 through 500"),
        (
            "search",
            json!({"pattern":"*", "limit":501}),
            "1 through 500",
        ),
    ];
    for format in ["text", "json"] {
        for (name, args, hint) in &cases {
            let mut args = args.clone();
            args["format"] = json!(format);
            let result = session.call(name, args).await?;
            assert!(
                tool_error(&result, format, "invalid_argument")?.contains(hint),
                "{name}"
            );
        }
        for (name, mut args) in [
            ("get_tree", json!({"path":"/bigdir/f000"})),
            ("top_files", json!({"under":"/bigdir/f000"})),
            ("ext_breakdown", json!({"under":"/bigdir/f000"})),
            ("search", json!({"pattern":"*", "under":"/bigdir/f000"})),
        ] {
            args["format"] = json!(format);
            let result = session.call(name, args).await?;
            assert!(tool_error(&result, format, "not_a_directory")?.contains("node_info"));
        }
        let result = session
            .call(
                "node_info",
                json!({"path":"/BIGDIR/not-found/child", "format":format}),
            )
            .await?;
        assert!(
            tool_error(&result, format, "path_not_found")?
                .contains("list_dir with path \"/bigdir\"")
        );
        let result = session
            .call(
                "get_tree",
                json!({"snapshot":"source.img", "format":format}),
            )
            .await?;
        assert!(tool_error(&result, format, "unsupported_source")?.contains("adamite scan"));
    }
    session.close().await
}

#[tokio::test(flavor = "current_thread")]
async fn corrupt_snapshots_retry_and_rewritten_snapshots_reload_without_restart() -> TestResult {
    let session = Session::new().await?;
    let path = session.directory.path().join("C.adm");
    let result = session
        .call("snapshot_info", json!({"format":"json"}))
        .await?;
    assert_eq!(
        structured(&result)?["snapshot"]["scanned_at"],
        "1970-01-01T00:00:00Z"
    );
    for format in ["text", "json"] {
        fs::write(&path, &session.output.to_bytes()?[..32])?;
        let result = session.call("get_tree", json!({"format":format})).await?;
        assert!(tool_error(&result, format, "corrupt_snapshot")?.contains("--force"));
        fs::write(&path, session.output.to_bytes()?)?;
        let result = session.call("get_tree", json!({"format":format})).await?;
        assert!(!result.is_error.unwrap_or(false));
    }
    let fixture = Fixture::load(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/ntfs/basic-4k.json"),
    )?;
    let output = SyncDriver::with_limits(
        ScanLimits::default(),
        ScanContext {
            scanned_at_filetime: Some(116_444_736_100_000_000),
            label: Some("replacement has a different length".into()),
            ..ScanContext::default()
        },
    )?
    .run(&mut SliceReader::new(fixture.image()))?;
    fs::write(&path, output.to_bytes()?)?;
    let result = session
        .call("snapshot_info", json!({"format":"json"}))
        .await?;
    assert_eq!(
        structured(&result)?["snapshot"]["scanned_at"],
        "1970-01-01T00:00:10Z"
    );
    session.close().await
}

#[tokio::test(flavor = "current_thread")]
async fn unreadable_snapshot_returns_permission_denied_and_can_be_retried() -> TestResult {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let session = Session::new().await?;
        let path = session.directory.path().join("C.adm");
        let permissions = fs::metadata(&path)?.permissions();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000))?;
        let enforced = matches!(fs::read(&path), Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied);
        let mut results = Vec::new();
        if enforced {
            for format in ["text", "json"] {
                results.push((
                    format,
                    session.call("get_tree", json!({"format":format})).await?,
                ));
            }
        }
        fs::set_permissions(&path, permissions)?;
        for (format, result) in results {
            assert!(tool_error(&result, format, "permission_denied")?.contains("Check access"));
        }
        assert!(
            !session
                .call("get_tree", json!({}))
                .await?
                .is_error
                .unwrap_or(false)
        );
        session.close().await?;
    }
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn wide_tree_budget_and_output_cap_preserve_whole_items_and_paging() -> TestResult {
    let session = Session::new().await?;
    let mut config = SyntheticConfig::reference(702)?;
    config.directory_percent = 0;
    config.name_units = 128;
    let mut image = Cursor::new(Vec::new());
    SyntheticVolume::new(config)?.write(&mut image)?;
    let output = SyncDriver::with_limits(ScanLimits::default(), ScanContext::default())?
        .run(&mut SliceReader::new(image.get_ref()))?;
    fs::write(session.directory.path().join("C.adm"), output.to_bytes()?)?;
    for budget in [1, 2, 7, 60, 400] {
        let result = session
            .call(
                "get_tree",
                json!({"budget":budget,"min_share_percent":0,"format":"json"}),
            )
            .await?;
        let tree = structured(&result)?;
        let rows = tree["rows"].as_array().ok_or("rows")?;
        assert!(!rows.is_empty());
        assert!(rows.len() <= budget);
        assert_eq!(rows[0]["path"], "/");
        assert_eq!(tree["truncated"], true);
        assert!(serde_json::to_vec(&result)?.len() <= 32768);
    }
    let result = session
        .call("get_tree", json!({"budget":7,"min_share_percent":0}))
        .await?;
    assert!(
        text(&result).contains("alloc   share  name"),
        "{}",
        text(&result)
    );
    assert!(text(&result).contains(" more ("));
    for name in ["list_dir", "search"] {
        for format in ["text", "json"] {
            let mut offset = 0;
            let mut seen = Vec::new();
            loop {
                let result = session
                    .call(
                        name,
                        json!({"format":format,"limit":500,"pattern":"*","offset":offset}),
                    )
                    .await?;
                assert!(!result.is_error.unwrap_or(false));
                assert!(serde_json::to_vec(&result)?.len() <= 32768);
                if format == "text" {
                    assert!(text(&result).contains("truncated"));
                    assert!(text(&result).contains("next offset="));
                    let lines: Vec<_> = text(&result).lines().collect();
                    for line in &lines[2..lines.len() - 2] {
                        let name = line.split_whitespace().last().ok_or("name")?;
                        assert!(
                            name.trim_start_matches('/').len() == 128
                                || line.contains("<NTFS metadata>"),
                            "partial item: {name}"
                        );
                    }
                    break;
                }
                let page = structured(&result)?;
                let items = page["items"].as_array().ok_or("items")?;
                assert!(!items.is_empty());
                assert!(items.iter().all(|item| item["name"].as_str().is_some()));
                seen.extend(
                    items
                        .iter()
                        .map(|item| item["name"].as_str().expect("name").to_owned()),
                );
                if offset == 0 {
                    assert_eq!(page["truncated"], true);
                    assert!(items.len() < 500);
                }
                if let Some(next) = page["next_offset"].as_u64() {
                    assert_eq!(next as usize, offset as usize + items.len());
                    offset = next;
                } else {
                    break;
                }
            }
            if format == "json" {
                let count = seen.len();
                seen.sort_unstable();
                seen.dedup();
                assert_eq!(seen.len(), count);
                let expected = if name == "list_dir" {
                    output
                        .snapshot
                        .children(adamite_core::snapshot::NodeId::ROOT)
                        .ok_or("children")?
                        .len()
                } else {
                    usize::try_from(
                        output
                            .snapshot
                            .search(
                                adamite_core::snapshot::NodeId::ROOT,
                                &adamite_core::snapshot::NamePattern::glob("*")?,
                                0,
                                0,
                            )?
                            .total_matches(),
                    )?
                };
                assert_eq!(count, expected);
            }
        }
    }
    session.close().await
}

#[tokio::test(flavor = "current_thread")]
async fn deep_tree_and_a_single_oversized_unicode_path_keep_valid_items() -> TestResult {
    use adamite_core::snapshot::NodeId;

    let session = Session::new().await?;
    let mut config = SyntheticConfig::reference(202)?;
    config.directory_percent = 100;
    let volume = SyntheticVolume::new(config)?;
    let mut image = Cursor::new(Vec::new());
    volume.write(&mut image)?;
    let layout = VolumeImage::new(
        BootSectorBuilder {
            mft: 256,
            ..BootSectorBuilder::default()
        },
        volume.len_bytes(),
    )?;
    let mut parent = 5;
    let mut deepest = String::new();
    for index in 0..200u32 {
        // SyntheticConfig::reference inserts one free MFT slot per four ordinary records.
        let record_number = 32 + index + index / 4;
        let name = format!(
            "{index:03}-{}{}",
            "目录".repeat(62),
            if index == 199 { ".txt" } else { "" }
        );
        deepest.push('/');
        deepest.push_str(&name);
        let filename = AttrBuilder::resident(
            0x30,
            FileNameBuilder {
                parent: (7u64 << 48) | parent,
                name: name.encode_utf16().collect(),
                ..FileNameBuilder::default()
            }
            .build(),
        )
        .build();
        let last = index == 199;
        let mut attributes = vec![filename];
        if last {
            attributes.push(AttrBuilder::resident(0x80, vec![1; 64]).build());
        }
        let record = RecordBuilder {
            record_number,
            flags: if last { 1 } else { 3 },
            attributes,
            ..RecordBuilder::default()
        };
        layout.write_record(&mut image, u64::from(record_number), &record.build())?;
        parent = u64::from(record_number);
    }
    assert!(deepest.len() > 32768);
    let output = SyncDriver::with_limits(ScanLimits::default(), ScanContext::default())?
        .run(&mut SliceReader::new(image.get_ref()))?;
    assert!(output.snapshot.resolve_path(&deepest)? != NodeId::ROOT);
    fs::write(session.directory.path().join("C.adm"), output.to_bytes()?)?;
    for format in ["text", "json"] {
        let result = session
            .call(
                "get_tree",
                json!({"budget":12,"min_share_percent":0,"format":format}),
            )
            .await?;
        assert!(!result.is_error.unwrap_or(false));
        if format == "json" {
            let tree = structured(&result)?;
            let rows = tree["rows"].as_array().ok_or("rows")?;
            assert!(rows.len() <= 12);
            assert!(
                rows.iter()
                    .any(|row| row["depth"].as_u64().is_some_and(|depth| depth > 2))
            );
            assert_eq!(tree["truncated"], true);
            if rows.len() == 12 {
                assert!(rows.iter().any(|row| row["kind"] == "collapsed"));
            }
        } else {
            assert!(text(&result).contains(" more ("));
            assert!(text(&result).contains("  000-"));
        }
        for (name, args) in [
            ("node_info", json!({"path":deepest,"format":format})),
            ("top_files", json!({"n":1,"ext":["txt"],"format":format})),
            (
                "search",
                json!({"pattern":"199-*","limit":1,"format":format}),
            ),
        ] {
            let result = session.call(name, args).await?;
            assert!(
                !result.is_error.unwrap_or(false),
                "{name}: {}",
                text(&result)
            );
            assert!(serde_json::to_vec(&result)?.len() <= 32768);
            assert!(
                text(&result).contains('…'),
                "{name}: path should be shortened"
            );
            if format == "json" {
                let report = structured(&result)?;
                assert_eq!(report["truncated"], true);
                let path = if name == "node_info" {
                    report["path"].as_str().ok_or("node path")?
                } else {
                    assert_eq!(report["items"].as_array().ok_or("items")?.len(), 1);
                    report["items"][0]["path"].as_str().ok_or("item path")?
                };
                assert!(path.starts_with("/000-"));
                assert!(path.ends_with("目录.txt"));
                assert!(path.contains('…'));
                assert!(report["next_offset"].is_null());
            }
        }
    }
    session.close().await
}

#[tokio::test(flavor = "current_thread")]
async fn catalog_breakdown_info_and_error_responses_share_the_output_cap() -> TestResult {
    let session = Session::new().await?;
    let mut config = SyntheticConfig::reference(202)?;
    config.directory_percent = 0;
    let volume = SyntheticVolume::new(config)?;
    let mut image = Cursor::new(Vec::new());
    volume.write(&mut image)?;
    let layout = VolumeImage::new(
        BootSectorBuilder {
            mft: 256,
            ..BootSectorBuilder::default()
        },
        volume.len_bytes(),
    )?;
    let mut extensions = Vec::new();
    for index in 0..200u32 {
        let record_number = 32 + index + index / 4;
        let extension = format!("{index:03}{}", "扩展".repeat(60));
        let filename = AttrBuilder::resident(
            0x30,
            FileNameBuilder {
                parent: (7u64 << 48) | 5,
                name: format!("f.{extension}").encode_utf16().collect(),
                ..FileNameBuilder::default()
            }
            .build(),
        )
        .build();
        let record = RecordBuilder {
            record_number,
            attributes: vec![filename, AttrBuilder::resident(0x80, vec![1; 64]).build()],
            ..RecordBuilder::default()
        };
        layout.write_record(&mut image, u64::from(record_number), &record.build())?;
        extensions.push(extension);
    }
    let output = SyncDriver::with_limits(ScanLimits::default(), ScanContext::default())?
        .run(&mut SliceReader::new(image.get_ref()))?;
    let path = session.directory.path().join("C.adm");
    fs::write(&path, output.to_bytes()?)?;
    for format in ["text", "json"] {
        let result = session
            .call("ext_breakdown", json!({"n":200,"format":format}))
            .await?;
        assert!(!result.is_error.unwrap_or(false));
        assert!(serde_json::to_vec(&result)?.len() <= 32768);
        if format == "json" {
            let report = structured(&result)?;
            assert_eq!(report["truncated"], true);
            let items = report["items"].as_array().ok_or("items")?;
            assert!(!items.is_empty());
            assert!(items.len() < 200);
            for item in items {
                let extension = item["extension"].as_str().ok_or("extension")?;
                assert!(
                    extension.is_empty() || extensions.iter().any(|expected| expected == extension)
                );
            }
        } else {
            assert!(text(&result).contains("truncated"));
            for line in text(&result)
                .lines()
                .skip(2)
                .filter(|line| *line != "truncated")
            {
                let extension = line.split_whitespace().last().ok_or("extension")?;
                assert!(
                    extension == "(none)"
                        || extensions.iter().any(|expected| expected == extension)
                );
            }
        }
        let result = session
            .call(
                "node_info",
                json!({"path":format!("/{}", "missing".repeat(10_000)),"format":format}),
            )
            .await?;
        tool_error(&result, format, "path_not_found")?;
        assert!(serde_json::to_vec(&result)?.len() <= 32768);
    }
    let fixture = Fixture::load(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/ntfs/basic-4k.json"),
    )?;
    let output = SyncDriver::with_limits(
        ScanLimits::default(),
        ScanContext {
            label: Some("label-目录".repeat(10_000)),
            ..ScanContext::default()
        },
    )?
    .run(&mut SliceReader::new(fixture.image()))?;
    fs::write(&path, output.to_bytes()?)?;
    for format in ["text", "json"] {
        let result = session
            .call("snapshot_info", json!({"format":format}))
            .await?;
        assert!(!result.is_error.unwrap_or(false));
        assert!(serde_json::to_vec(&result)?.len() <= 32768);
        if format == "json" {
            assert_eq!(structured(&result)?["truncated"], true);
        } else {
            assert!(text(&result).contains("truncated"));
        }
    }
    for index in 0..150 {
        fs::write(
            session
                .directory
                .path()
                .join(format!("{index:03}{}.adm", "x".repeat(128))),
            [],
        )?;
    }
    for format in ["text", "json"] {
        let result = session.call("snapshots", json!({"format":format})).await?;
        assert!(!result.is_error.unwrap_or(false));
        assert!(serde_json::to_vec(&result)?.len() <= 32768);
        if format == "json" {
            let catalog = structured(&result)?;
            assert_eq!(catalog["truncated"], true);
            let items = catalog["items"].as_array().ok_or("items")?;
            assert!(!items.is_empty());
            assert!(items.len() < 151);
            assert!(items.iter().all(|item| {
                item["path"]
                    .as_str()
                    .is_some_and(|path| path.ends_with(".adm"))
            }));
        } else {
            assert!(text(&result).contains("truncated"));
        }
    }
    session.close().await
}

#[tokio::test(flavor = "current_thread")]
async fn node_metadata_preserves_named_flags_hard_links_and_recorded_timestamps() -> TestResult {
    let session = Session::new().await?;
    for (path, flag) in [
        ("/sparse.bin", "sparse"),
        ("/compressed/c.txt", "compressed"),
        ("/hidden.txt", "hidden"),
        ("/hl/orig.txt", ""),
        ("/bigdir", "directory"),
    ] {
        let result = session
            .call("node_info", json!({"path":path,"format":"json"}))
            .await?;
        let node = structured(&result)?;
        let id = session.output.snapshot.resolve_path(path)?;
        let view = session.output.snapshot.node(id).ok_or("node")?;
        assert_eq!(node["node_id"], id.raw());
        assert_eq!(node["hard_link_count"], view.nlink());
        assert_eq!(node["size"], view.size());
        assert_eq!(node["alloc"], view.alloc());
        assert_eq!(
            node["flags"],
            json!(view.flags().names().collect::<Vec<_>>())
        );
        if !flag.is_empty() {
            assert!(
                node["flags"]
                    .as_array()
                    .ok_or("flags")?
                    .contains(&json!(flag))
            );
        }
        if view.mtime() != 0 {
            assert_eq!(node["mtime_filetime"], view.mtime().to_string());
        }
        if path == "/hl/orig.txt" {
            assert!(
                node["hard_link_count"]
                    .as_u64()
                    .is_some_and(|count| count > 1)
            );
        }
        if view.is_dir() {
            assert_eq!(node["total_size"], view.total_size());
            assert_eq!(node["total_alloc"], view.total_alloc());
        }
    }
    for format in ["text", "json"] {
        let result = session
            .call(
                "get_tree",
                json!({"path":"/d00","budget":8,"min_share_percent":0,"format":format}),
            )
            .await?;
        assert!(!result.is_error.unwrap_or(false));
        if format == "json" {
            let tree = structured(&result)?;
            let rows = tree["rows"].as_array().ok_or("rows")?;
            assert!(rows.len() <= 8);
            assert!(rows.iter().any(|row| row["kind"] == "collapsed"));
            assert!(
                rows.iter()
                    .any(|row| row["depth"].as_u64().is_some_and(|depth| depth > 2))
            );
        } else {
            assert!(text(&result).contains(" more ("));
            assert!(text(&result).contains("      d03"));
        }
    }
    session.close().await
}

impl Session {
    fn image(&self, name: &str) -> TestResult<std::path::PathBuf> {
        let fixture = Fixture::load(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/ntfs/basic-4k.json"),
        )?;
        let path = self.directory.path().join(name);
        fs::write(&path, fixture.image())?;
        Ok(fs::canonicalize(path)?)
    }

    async fn scan_finished(&self) -> TestResult<CallToolResult> {
        let started = std::time::Instant::now();
        loop {
            let result = self.call("scan_status", json!({"format":"json"})).await?;
            if result.is_error == Some(true) || structured(&result)?["status"] != "running" {
                return Ok(result);
            }
            assert!(
                started.elapsed() < Duration::from_secs(20),
                "scan timed out"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn scan_image_publishes_discovers_queries_and_retains_done() -> TestResult {
    let session = Session::new().await?;
    for format in ["text", "json"] {
        let idle = session
            .call("scan_status", json!({"format":format}))
            .await?;
        assert!(!idle.is_error.unwrap_or(false));
        if format == "json" {
            assert_eq!(structured(&idle)?["status"], "idle");
        } else {
            assert_eq!(text(&idle), "idle\n");
        }
    }
    let image = session.image("source image.img")?;
    let target = session.directory.path().join("source image.adm");
    fs::write(&target, "old snapshot")?;
    for profile in ["balanced", "background", "max"] {
        let running = session
            .call(
                "scan_volume",
                json!({"volume":image,"profile":profile,"format":"json"}),
            )
            .await?;
        assert!(!running.is_error.unwrap_or(false), "{}", text(&running));
        let running = structured(&running)?;
        assert_eq!(running["status"], "running");
        assert_eq!(running["volume"], json!(image));
        assert_eq!(running["snapshot"]["path"], json!(target));
        let done = session.scan_finished().await?;
        assert!(!done.is_error.unwrap_or(false), "{}", text(&done));
        let done = structured(&done)?;
        assert_eq!(done["status"], "done");
        assert_eq!(done["snapshot"]["path"], json!(target));
        for _ in 0..3 {
            let repeated = session
                .call("scan_status", json!({"volume":image,"format":"json"}))
                .await?;
            assert_eq!(structured(&repeated)?, done);
        }
        let plain = session.call("scan_status", json!({})).await?;
        assert!(text(&plain).starts_with("done · "));
        assert!(text(&plain).contains(target.to_str().ok_or("target path")?));
        let catalog = session.call("snapshots", json!({"format":"json"})).await?;
        assert!(
            structured(&catalog)?["items"]
                .as_array()
                .ok_or("items")?
                .iter()
                .any(|item| item["path"] == json!(target))
        );
        let tree = session
            .call("get_tree", json!({"snapshot":target,"format":"json"}))
            .await?;
        assert!(!tree.is_error.unwrap_or(false), "{}", text(&tree));
        assert!(
            !structured(&tree)?["rows"]
                .as_array()
                .ok_or("rows")?
                .is_empty()
        );
        let statuses =
            fs::read_to_string(session.directory.path().join(".source image.status.jsonl"))?;
        let final_line: Value =
            serde_json::from_str(statuses.lines().last().ok_or("terminal status")?)?;
        assert_eq!(final_line["done"]["saved"], json!(target));
    }
    session.close().await
}

#[tokio::test(flavor = "current_thread")]
async fn scan_rejects_invalid_sources_and_volume_mismatches() -> TestResult {
    let session = Session::new().await?;
    for format in ["text", "json"] {
        for (volume, code) in [
            (session.directory.path().join("C.adm"), "unsupported_source"),
            (
                session.directory.path().join("missing.img"),
                "invalid_argument",
            ),
            (session.directory.path().to_owned(), "invalid_argument"),
            (
                session.directory.path().join("C: --force"),
                "invalid_argument",
            ),
        ] {
            let result = session
                .call("scan_volume", json!({"volume":volume,"format":format}))
                .await?;
            let message = tool_error(&result, format, code)?;
            if code == "unsupported_source" {
                assert!(message.contains("Query it with get_tree"));
            }
        }
        if !cfg!(windows) {
            for volume in ["C", "c:"] {
                let result = session
                    .call("scan_volume", json!({"volume":volume,"format":format}))
                    .await?;
                let message = tool_error(&result, format, "unsupported_source")?;
                assert!(message.contains("adamite scan \"<image-file>\""));
            }
        }
        let mismatch = session
            .call("scan_status", json!({"volume":"C:","format":format}))
            .await?;
        tool_error(&mismatch, format, "invalid_argument")?;
    }
    assert!(
        session
            .call("scan_volume", json!({"volume":"C:","profile":"injected"}))
            .await
            .is_err()
    );
    let image = session.image("valid.img")?;
    let other = session.image("other.img")?;
    session.call("scan_volume", json!({"volume":image})).await?;
    let done = session.scan_finished().await?;
    assert!(!done.is_error.unwrap_or(false));
    let mismatch = session.call("scan_status", json!({"volume":other})).await?;
    tool_error(&mismatch, "text", "invalid_argument")?;
    session.close().await
}

#[tokio::test(flavor = "current_thread")]
async fn scan_failure_keeps_message_and_repeated_terminal_status_and_can_restart() -> TestResult {
    let session = Session::new().await?;
    let image = session.directory.path().join("corrupt.img");
    fs::write(&image, "not an NTFS volume")?;
    let started = session.call("scan_volume", json!({"volume":image})).await?;
    assert!(!started.is_error.unwrap_or(false));
    assert!(text(&started).starts_with("running"));
    let failed = session.scan_finished().await?;
    let message = tool_error(&failed, "json", "scan_failed")?;
    assert!(
        message.contains("cannot scan") && message.contains("corrupt.img"),
        "{message}"
    );
    assert!(message.contains("exit code"));
    assert!(!message.contains("worker stopped without a result"));
    for _ in 0..3 {
        let repeated = session
            .call("scan_status", json!({"format":"json"}))
            .await?;
        assert_eq!(structured(&repeated)?, structured(&failed)?);
    }
    let plain = session.call("scan_status", json!({})).await?;
    tool_error(&plain, "text", "scan_failed")?;
    let image = session.image("corrupt.img")?;
    let running = session.call("scan_volume", json!({"volume":image})).await?;
    assert!(!running.is_error.unwrap_or(false), "{}", text(&running));
    let done = session.scan_finished().await?;
    assert_eq!(structured(&done)?["status"], "done");
    session.close().await
}

#[tokio::test(flavor = "current_thread")]
async fn scan_busy_and_progress_use_the_running_job() -> TestResult {
    let session = Session::new().await?;
    let image = session.directory.path().join("large.img");
    let volume = SyntheticVolume::new(SyntheticConfig::reference(100_002)?)?;
    let mut file = fs::File::create(&image)?;
    volume.write(&mut file)?;
    drop(file);
    let running = session
        .call(
            "scan_volume",
            json!({"volume":image,"profile":"background","format":"json"}),
        )
        .await?;
    assert_eq!(structured(&running)?["status"], "running");
    let busy = session.call("scan_volume", json!({"volume":image})).await?;
    let message = tool_error(&busy, "text", "scan_busy")?;
    assert!(message.contains("Call scan_status"));
    let mut observed_progress = false;
    loop {
        let progress = session
            .call("scan_status", json!({"format":"json"}))
            .await?;
        assert!(!progress.is_error.unwrap_or(false), "{}", text(&progress));
        let progress = structured(&progress)?;
        if progress["status"] == "done" {
            break;
        }
        assert_eq!(progress["status"], "running");
        assert!(progress["phase"].is_string());
        assert!(
            progress["elapsed_seconds"]
                .as_f64()
                .is_some_and(|elapsed| elapsed >= 0.0)
        );
        assert!(progress["bytes_read"].is_u64());
        if let (Some(records), Some(planned)) = (
            progress["records_processed"].as_u64(),
            progress["records_planned"].as_u64(),
        ) && planned > 0
        {
            let expected = records as f64 / planned as f64 * 100.0;
            assert!((progress["percentage"].as_f64().ok_or("percentage")? - expected).abs() < 1e-9);
            observed_progress |= progress["bytes_read"]
                .as_u64()
                .is_some_and(|bytes| bytes > 0);
        }
        assert!(progress["elapsed_seconds"].as_f64().ok_or("elapsed")? < 20.0);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        observed_progress,
        "large image should report progress before completion"
    );
    session.close().await
}

#[tokio::test(flavor = "current_thread")]
async fn rejected_status_collision_is_preserved_on_subsequent_launches() -> TestResult {
    let session = Session::new().await?;
    let image = session.image("collision.img")?;
    let status = session.directory.path().join(".collision.status.jsonl");
    fs::write(&status, "existing job status")?;
    for _ in 0..2 {
        let result = session.call("scan_volume", json!({"volume":image})).await?;
        tool_error(&result, "text", "scan_failed")?;
        assert_eq!(fs::read_to_string(&status)?, "existing job status");
    }
    let idle = session
        .call("scan_status", json!({"format":"json"}))
        .await?;
    assert_eq!(structured(&idle)?["status"], "idle");
    session.close().await
}
