//! Process-level coverage of source detection, reports, snapshot saving and command failures.

mod support;

use adamite_core::{
    scan::{ScanConfig, ScanContext, ScanOutput, ScanStats},
    snapshot::{FileFilter, FileKind, NamePattern, NodeId, Snapshot},
};
use adamite_fixtures::{
    Fixture,
    builders::{AttrBuilder, BootSectorBuilder, FileNameBuilder, NonResidentModel, RecordBuilder},
    runlist_encoder::{RunModel, RunlistEncoder},
};
use adamite_io::{driver::SyncDriver, reader::SliceReader};
use serde_json::{Value, json};
use std::{
    error::Error,
    fs,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

use support::TestResult;

struct Case {
    directory: PathBuf,
    image: PathBuf,
    output: ScanOutput,
}

impl Case {
    fn new(name: &str, fixture_name: &str) -> Result<Self, Box<dyn Error>> {
        let fixture = Fixture::load(
            &Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(format!("../../tests/fixtures/ntfs/{fixture_name}.json")),
        )?;
        Self::from_image(name, fixture.image())
    }

    fn from_image(name: &str, bytes: &[u8]) -> Result<Self, Box<dyn Error>> {
        let directory = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
        fs::create_dir_all(&directory)?;
        let image = directory.join("source.img");
        fs::write(&image, bytes)?;
        let mut reader = SliceReader::new(bytes);
        let output = SyncDriver::new(ScanConfig::default())?.run(&mut reader)?;
        Ok(Self {
            directory,
            image,
            output,
        })
    }

    fn command(&self, name: &str, source: &Path, flags: &[&str]) -> Output {
        support::command(name, source, flags)
    }

    fn json(&self, name: &str, source: &Path) -> Value {
        self.json_with(name, source, &[])
    }

    fn json_with(&self, name: &str, source: &Path, flags: &[&str]) -> Value {
        support::json_with(name, source, flags)
    }

    fn path_id(&self, path: &str) -> NodeId {
        let snapshot = &self.output.snapshot;
        (0..snapshot.len())
            .map(|raw| NodeId::from_raw(u32::try_from(raw).expect("fixture id")))
            .find(|&id| snapshot.path(id).expect("valid path").as_deref() == Some(path))
            .expect("fixture path")
    }

    fn assert_command_failure(&self, name: &str, flags: &[&str], code: i32) -> String {
        let output = self.command(name, &self.image, flags);
        assert_eq!(output.status.code(), Some(code), "{output:?}");
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8(output.stderr).expect("stderr text");
        assert!(
            stderr.lines().any(|line| line.starts_with("error: ")),
            "{stderr}"
        );
        assert!(!stderr.contains("panicked"), "{stderr}");
        stderr
    }

    fn snapshot(&self, name: &str) -> Result<PathBuf, Box<dyn Error>> {
        let path = self.directory.join(name);
        fs::write(&path, self.output.to_bytes()?)?;
        Ok(path)
    }

    fn save(&self, destination: &Path, flags: &[&str]) -> Output {
        support::save(&self.image, destination, flags)
    }

    fn assert_no_temporary_files(&self) -> TestResult {
        for entry in fs::read_dir(&self.directory)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            assert!(
                !(name.starts_with('.') && name.ends_with(".tmp")),
                "temporary file remains: {}",
                entry.path().display()
            );
        }
        Ok(())
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
    Value::Object(
        stats
            .counters()
            .into_iter()
            .map(|(name, value)| (name.to_owned(), Value::from(value)))
            .collect(),
    )
}

fn without_source_and_time(mut value: Value) -> Value {
    let snapshot = value["snapshot"].as_object_mut().expect("snapshot object");
    for field in ["source", "scanned_at", "scanned_at_filetime"] {
        snapshot.remove(field);
    }
    value
}

fn expected_ls_item(snapshot: &Snapshot, id: NodeId) -> Value {
    let node = snapshot.node(id).expect("child node");
    let flags: Vec<_> = node.flags().names().collect();
    json!({
        "name": snapshot.name(id).expect("child name"),
        "kind": if node.is_dir() { "dir" } else { "file" },
        "flags": flags,
        "size": node.size(), "alloc": node.alloc(),
        "total_size": node.total_size(), "total_alloc": node.total_alloc(),
        "file_count": node.file_count(),
    })
}

/// Independent encoders retain duplicate names that normal filesystem tools reject.
fn path_test_image() -> Vec<u8> {
    let mut image = vec![0; 128 * 512];
    image[..512].copy_from_slice(
        &BootSectorBuilder {
            cluster_encoding: 1,
            sectors: 128,
            mirror: 64,
            ..BootSectorBuilder::default()
        }
        .build(),
    );
    let name = |parent: u32, text: &str| {
        AttrBuilder::resident(
            0x30,
            FileNameBuilder {
                parent: (7 << 48) | u64::from(parent),
                namespace: 0,
                name: text.encode_utf16().collect(),
                ..FileNameBuilder::default()
            }
            .build(),
        )
        .build()
    };
    let mft = AttrBuilder::non_resident(
        0x80,
        NonResidentModel {
            highest: 63,
            sizes: [32768; 4],
            mapping_pairs: RunlistEncoder {
                runs: vec![RunModel {
                    lcn: Some(4),
                    len: 64,
                }],
            }
            .encode(0),
            ..NonResidentModel::default()
        },
    )
    .build();
    let mut records = vec![
        RecordBuilder {
            record_number: 0,
            attributes: vec![mft, name(5, "$MFT")],
            ..RecordBuilder::default()
        },
        RecordBuilder {
            record_number: 5,
            flags: 3,
            attributes: vec![name(5, ".")],
            ..RecordBuilder::default()
        },
    ];
    for (index, text) in [
        "Twin",
        "twin",
        "same",
        "same",
        "İ",
        ".",
        "..",
        "line\n\t\u{1b}.TX\nT",
    ]
    .iter()
    .enumerate()
    {
        records.push(RecordBuilder {
            record_number: 16 + u32::try_from(index).expect("bounded record"),
            flags: 3,
            attributes: vec![name(5, text)],
            ..RecordBuilder::default()
        });
    }
    records.push(RecordBuilder {
        record_number: 24,
        attributes: vec![
            name(23, "file\n.tx\nt"),
            AttrBuilder::resident(0x80, b"hi".to_vec()).build(),
        ],
        ..RecordBuilder::default()
    });
    for record in records {
        let start = 4 * 512 + record.record_number as usize * 1024;
        image[start..start + 1024].copy_from_slice(&record.build());
    }
    image
}

#[test]
fn ambiguous_paths_require_a_unique_name_or_direct_child_id() -> TestResult {
    let case = Case::from_image("path-ambiguity", &path_test_image())?;
    let snapshot = &case.output.snapshot;
    for path in ["/Twin", "/twin", "/i\u{307}", "/.", "/.."] {
        let value = case.json_with("ls", &case.image, &[path]);
        let expected = if path == "/i\u{307}" { "/İ" } else { path };
        assert_eq!(value["path"], expected);
    }
    for (path, names) in [
        ("/TWIN", vec!["Twin", "twin"]),
        ("/same", vec!["same", "same"]),
    ] {
        let stderr = case.assert_command_failure("ls", &[path, "--json"], 1);
        assert!(stderr.contains("ambiguous path component"), "{stderr}");
        assert!(stderr.contains(path.trim_start_matches('/')), "{stderr}");
        let ids: Vec<_> = snapshot
            .children(NodeId::ROOT)
            .ok_or("root children")?
            .iter()
            .filter(|&&id| names.contains(&snapshot.name(id).expect("name")))
            .copied()
            .collect();
        assert_eq!(ids.len(), 2);
        for id in ids {
            assert!(
                stderr.contains(&format!("NodeId({})", id.raw())),
                "{stderr}"
            );
            let selected = format!("/#{}", id.raw());
            let value = case.json_with("ls", &case.image, &[&selected]);
            assert_eq!(value["path"], snapshot.path(id)?.ok_or("selected path")?);
            assert_eq!(value["items"], json!([]));
        }
    }
    let saved = case.snapshot("synthetic.adm")?;
    assert!(
        case.assert_command_failure("ls", &["/Twin/..", "--json"], 1)
            .contains("not found")
    );
    for source in [&case.image, &saved] {
        let root = case.command("ls", source, &[]);
        assert!(root.status.success(), "{root:?}");
        let text = String::from_utf8(root.stdout)?;
        assert!(
            text.contains("line\\u{a}\\u{9}\\u{1b}.TX\\u{a}T/"),
            "{text}"
        );
        assert!(!text.contains('\u{1b}'), "{text}");
        let path = "/line\n\t\u{1b}.TX\nT";
        let value = case.json_with("ls", source, &[path]);
        assert_eq!(value["path"], path);
        assert_eq!(value["items"][0]["name"], "file\n.tx\nt");
        let selected = format!("/#{}", case.path_id(path).raw());
        assert_eq!(case.json_with("ls", source, &[&selected])["path"], path);
        for command in ["top", "ext"] {
            let output = case.command(command, source, &["--n", "100"]);
            assert!(output.status.success(), "{output:?}");
            let text = String::from_utf8(output.stdout)?;
            if command == "top" {
                assert!(
                    text.contains("/line\\u{a}\\u{9}\\u{1b}.TX\\u{a}T/file\\u{a}.tx\\u{a}t"),
                    "{text}"
                );
            } else {
                assert!(text.contains("tx\\u{a}t"), "{text}");
            }
            assert!(!text.contains('\u{1b}'), "{text}");
        }
        let top = case.json("top", source);
        let file = top["items"]
            .as_array()
            .ok_or("top items")?
            .iter()
            .find(|item| item["path"] == format!("{path}/file\n.tx\nt"))
            .ok_or("synthetic top file")?;
        assert_eq!(file["mtime_filetime"], "0");
    }
    Ok(())
}

#[test]
fn query_json_matches_in_process_contents_and_order() -> TestResult {
    for fixture in ["basic-4k", "frag-512"] {
        let case = Case::new(&format!("query-json-{fixture}"), fixture)?;
        let snapshot = &case.output.snapshot;
        for n in [1, 5, 20, 100] {
            let value = case.json_with("top", &case.image, &["--n", &n.to_string(), "--bytes"]);
            let expected: Vec<_> = snapshot
                .top_files()
                .iter()
                .take(n)
                .map(|&id| {
                    let node = snapshot.node(id).expect("top file");
                    json!({
                        "path": snapshot.path(id).expect("file path").expect("existing file"),
                        "size": node.size(), "alloc": node.alloc(),
                        "mtime_filetime": node.mtime().to_string(),
                    })
                })
                .collect();
            assert_eq!(value["items"], json!(expected), "top {fixture}, n={n}");
            assert_eq!(value["command"], "top");
            assert_eq!(value["schema"], "adamite/1");
            assert_eq!(value["units"], json!({"size": "bytes"}));
            assert_eq!(value.as_object().expect("top object").len(), 5);
        }
        for n in [1, 5, 20, 1000] {
            let value = case.json_with("ext", &case.image, &["--n", &n.to_string()]);
            let expected: Vec<_> = snapshot
                .ext_stats()
                .iter()
                .take(n)
                .map(|stat| {
                    json!({
                        "extension": stat.extension(), "count": stat.count(),
                        "total_size": stat.total_size(), "total_alloc": stat.total_alloc(),
                    })
                })
                .collect();
            assert_eq!(value["items"], json!(expected), "ext {fixture}, n={n}");
            assert_eq!(value["command"], "ext");
            assert_eq!(value.as_object().expect("ext object").len(), 5);
        }
        for raw in 0..snapshot.len() {
            let id = NodeId::from_raw(u32::try_from(raw)?);
            if !snapshot.node(id).ok_or("node")?.is_dir() {
                continue;
            }
            let path = snapshot.path(id)?.ok_or("directory path")?;
            let children = snapshot.children(id).ok_or("directory children")?;
            for (offset, limit) in [(0, 50), (1, 3)] {
                let value = case.json_with(
                    "ls",
                    &case.image,
                    &[
                        &path,
                        "--offset",
                        &offset.to_string(),
                        "--limit",
                        &limit.to_string(),
                    ],
                );
                let expected: Vec<_> = children
                    .iter()
                    .skip(offset)
                    .take(limit)
                    .map(|&id| expected_ls_item(snapshot, id))
                    .collect();
                assert_eq!(value["items"], json!(expected), "ls {fixture}: {path}");
                assert_eq!(value["path"], path);
                assert_eq!(value["total_children"], children.len());
                assert_eq!(value["offset"], offset);
                assert_eq!(value["limit"], limit);
                assert_eq!(value["command"], "ls");
                assert_eq!(value.as_object().expect("ls object").len(), 9);
            }
        }
    }
    Ok(())
}

#[test]
fn saved_queries_match_image_queries() -> TestResult {
    for fixture in ["basic-4k", "frag-512"] {
        let case = Case::new(&format!("saved-queries-{fixture}"), fixture)?;
        let path = case.directory.join("queries.adm");
        let saved = case.save(&path, &["--json"]);
        assert!(saved.status.success(), "{saved:?}");
        for command in ["top", "ext", "ls"] {
            assert_eq!(
                without_source_and_time(case.json(command, &path)),
                without_source_and_time(case.json(command, &case.image)),
                "{command} {fixture}"
            );
        }
    }
    Ok(())
}

#[test]
fn ls_paths_and_pagination() -> TestResult {
    let case = Case::new("ls-paths", "basic-4k")?;
    for path in [
        "bigdir",
        "/bigdir/",
        "//BIGDIR//",
        "\\bigdir\\",
        "/\\BiGdIr\\/",
    ] {
        let value = case.json_with("ls", &case.image, &[path]);
        assert_eq!(value["path"], "/bigdir", "{path}");
        assert_eq!(value["total_children"], 200);
        assert_eq!(value["items"].as_array().ok_or("items")?.len(), 50);
    }
    assert_eq!(case.json("ls", &case.image)["path"], "/");
    for path in ["/", "\\", "", "////\\\\"] {
        assert_eq!(case.json_with("ls", &case.image, &[path])["path"], "/");
    }
    let id = case.path_id("/bigdir");
    let selected = format!("/#{}/", id.raw());
    assert_eq!(
        case.json_with("ls", &case.image, &[&selected])["path"],
        "/bigdir"
    );
    let metadata = case.json_with("ls", &case.image, &["/<ntfs METADATA>"]);
    assert_eq!(metadata["path"], "/<NTFS metadata>");
    for path in [
        "/missing",
        "/bigdir/missing",
        "/#4294967295",
        "/#0",
        "/#bad",
        "/#",
        "/#-1",
        "/#+1",
        "/#4294967296",
    ] {
        assert!(
            case.assert_command_failure("ls", &[path, "--json"], 1)
                .contains("not found")
        );
    }
    let nested_id = case.path_id("/bigdir/f000");
    let nested = format!("/#{}", nested_id.raw());
    assert!(
        case.assert_command_failure("ls", &[&nested, "--json"], 1)
            .contains("not found")
    );
    for path in [
        "/tiny.txt",
        "/tiny.txt/child",
        &format!("/bigdir/#{id}", id = nested_id.raw()),
    ] {
        assert!(
            case.assert_command_failure("ls", &[path, "--json"], 1)
                .contains("not a directory")
        );
    }
    for (offset, limit) in [(200, 50), (1000, 50), (0, 0), (usize::MAX, 0)] {
        let value = case.json_with(
            "ls",
            &case.image,
            &[
                "/bigdir",
                "--offset",
                &offset.to_string(),
                "--limit",
                &limit.to_string(),
            ],
        );
        assert_eq!(value["items"], json!([]));
        assert_eq!(value["total_children"], 200);
        assert_eq!(value["offset"], offset);
        assert_eq!(value["limit"], limit);
    }
    Ok(())
}

#[test]
fn query_numeric_arguments_are_checked() -> TestResult {
    let case = Case::new("query-arguments", "basic-4k")?;
    for n in ["0", "101", "-1", "no", "4294967296"] {
        let stderr = case.assert_command_failure("top", &["--n", n], 2);
        assert!(stderr.contains("100-entry index"), "{stderr}");
    }
    for (command, flag, value) in [
        ("ext", "--n", "0"),
        ("ext", "--n", "-1"),
        ("ext", "--n", "x"),
        ("ext", "--n", "18446744073709551616"),
        ("ls", "--offset", "-1"),
        ("ls", "--offset", "x"),
        ("ls", "--limit", "-1"),
        ("ls", "--limit", "1.5"),
        ("ls", "--offset", "18446744073709551616"),
        ("ls", "--limit", "18446744073709551616"),
    ] {
        case.assert_command_failure(command, &[flag, value, "--json"], 2);
    }
    assert!(
        case.assert_command_failure(
            "ls",
            &[
                "--offset",
                &usize::MAX.to_string(),
                "--limit",
                "1",
                "--json",
            ],
            2
        )
        .contains("--offset + --limit")
    );
    assert_eq!(
        without_source_and_time(case.json_with(
            "ext",
            &case.image,
            &["--n", &u64::MAX.to_string()]
        )),
        without_source_and_time(case.json("ext", &case.image))
    );
    Ok(())
}

#[test]
fn basic_query_text_has_aligned_columns_and_directory_markers() -> TestResult {
    let case = Case::new("query-text", "basic-4k")?;
    let top = concat!(
        "        Size        Alloc Path\n",
        "     2.0 MiB      2.0 MiB /<NTFS metadata>/$LogFile\n",
        "   323.0 KiB    336.0 KiB /<NTFS metadata>/$MFT\n",
        "         0 B    260.0 KiB /<NTFS metadata>/$Secure\n",
        "   128.0 KiB    128.0 KiB /<NTFS metadata>/$UpCase\n",
        "    64.0 KiB     64.0 KiB /medium.bin\n",
    );
    let ext = concat!(
        "        Size        Alloc      Count Extension\n",
        "     2.5 MiB      2.7 MiB        217 (none)\n",
        "     1.1 MiB     72.0 KiB          3 bin\n",
        "   256.2 KiB     52.0 KiB         11 txt\n",
    );
    let long_name = "n".repeat(251);
    let ls = format!(
        concat!(
            "  Total size  Total alloc      Files Name\n",
            "     2.5 MiB      2.7 MiB         17 <NTFS metadata>/\n",
            "    64.0 KiB     64.0 KiB          1 medium.bin\n",
            "         0 B     40.0 KiB        200 bigdir/\n",
            "   256.0 KiB     32.0 KiB          1 compressed/\n",
            "         0 B     20.0 KiB          1 ads.txt\n",
            "     1.0 MiB      8.0 KiB          1 sparse.bin\n",
            "         1 B          0 B          1 LongFileName.txt\n",
            "         1 B          0 B          1 d00/\n",
            "         1 B          0 B          1 emoji-😀.txt\n",
            "         0 B          0 B          1 empty.txt\n",
            "         1 B          0 B          1 hidden.txt\n",
            "       100 B          0 B          1 hl/\n",
            "         1 B          0 B          1 {}.txt\n",
            "         0 B          0 B          0 other/\n",
            "         0 B          0 B          1 reparse.bin\n",
            "       100 B          0 B          1 tiny.txt\n",
            "         1 B          0 B          1 日本語.txt\n",
        ),
        long_name
    );
    let top_bytes = concat!(
        "                Size                Alloc Path\n",
        "             2097152              2097152 /<NTFS metadata>/$LogFile\n",
        "              330752               344064 /<NTFS metadata>/$MFT\n",
        "                   0               266240 /<NTFS metadata>/$Secure\n",
        "              131072               131072 /<NTFS metadata>/$UpCase\n",
        "               65536                65536 /medium.bin\n",
    );
    let ext_bytes = concat!(
        "                Size                Alloc      Count Extension\n",
        "             2574336              2859008        217 (none)\n",
        "             1114112                73728          3 bin\n",
        "              262350                53248         11 txt\n",
    );
    let ls_bytes = format!(
        concat!(
            "          Total size          Total alloc      Files Name\n",
            "             2574336              2859008         17 <NTFS metadata>/\n",
            "               65536                65536          1 medium.bin\n",
            "                   0                40960        200 bigdir/\n",
            "              262144                32768          1 compressed/\n",
            "                   0                20480          1 ads.txt\n",
            "             1048576                 8192          1 sparse.bin\n",
            "                   1                    0          1 LongFileName.txt\n",
            "                   1                    0          1 d00/\n",
            "                   1                    0          1 emoji-😀.txt\n",
            "                   0                    0          1 empty.txt\n",
            "                   1                    0          1 hidden.txt\n",
            "                 100                    0          1 hl/\n",
            "                   1                    0          1 {}.txt\n",
            "                   0                    0          0 other/\n",
            "                   0                    0          1 reparse.bin\n",
            "                 100                    0          1 tiny.txt\n",
            "                   1                    0          1 日本語.txt\n",
        ),
        long_name
    );
    for (command, flags, expected) in [
        ("top", vec!["--n", "5"], top),
        ("ext", vec!["--n", "5"], ext),
        ("ls", vec!["/"], ls.as_str()),
        ("top", vec!["--n", "5", "--bytes"], top_bytes),
        ("ext", vec!["--n", "5", "--bytes"], ext_bytes),
        ("ls", vec!["/", "--bytes"], ls_bytes.as_str()),
    ] {
        let output = case.command(command, &case.image, &flags);
        assert!(output.status.success(), "{output:?}");
        let text = String::from_utf8(output.stdout)?;
        let (header, body) = text.split_once('\n').ok_or("missing header")?;
        assert!(header.starts_with(&format!("{} — scanned ", case.image.display())));
        assert_eq!(body, expected, "{command} {flags:?}");
    }
    Ok(())
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
fn saved_scan_round_trips_and_matches_the_image() -> TestResult {
    for fixture in ["basic-4k", "frag-512"] {
        let case = Case::new(&format!("save-round-trip-{fixture}"), fixture)?;
        let path = case.directory.join("saved.adm");
        let output = case.save(&path, &["--json"]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value: Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(value["schema"], "adamite/1");
        assert_eq!(value["command"], "scan");
        assert_eq!(value["units"], json!({"size": "bytes"}));
        assert_eq!(value["saved"], path.to_string_lossy().as_ref());
        assert_eq!(
            value["snapshot"]["source"],
            case.image.to_string_lossy().as_ref()
        );
        assert_eq!(value["bytes"], fs::metadata(&path)?.len());
        assert_eq!(value.as_object().ok_or("save report")?.len(), 6);
        let saved_info = case.json("info", &path);
        assert_eq!(
            saved_info["snapshot"]["source"],
            path.to_string_lossy().as_ref()
        );
        assert_eq!(
            saved_info["snapshot"]["scanned_at"],
            value["snapshot"]["scanned_at"]
        );
        assert_eq!(
            saved_info["snapshot"]["scanned_at_filetime"],
            value["snapshot"]["scanned_at_filetime"]
        );
        assert_eq!(
            without_source_and_time(saved_info),
            without_source_and_time(case.json("info", &case.image))
        );
        let decoded = ScanOutput::from_bytes(&fs::read(&path)?)?;
        let timestamp = decoded.snapshot.volume().context().scanned_at_filetime;
        assert!(timestamp.is_some(), "saved scan timestamp");
        let config = ScanConfig {
            context: ScanContext {
                scanned_at_filetime: timestamp,
                ..ScanContext::default()
            },
            ..ScanConfig::default()
        };
        let image = fs::read(&case.image)?;
        let expected = SyncDriver::new(config)?.run(&mut SliceReader::new(&image))?;
        assert_eq!(decoded, expected, "saved scan differs from in-process scan");
        case.assert_no_temporary_files()?;
    }
    Ok(())
}

#[test]
fn saved_scan_text_is_one_line_and_honors_bytes() -> TestResult {
    let case = Case::new("save-text", "basic-4k")?;
    let path = case.directory.join("saved.adm");
    let output = Command::new(env!("CARGO_BIN_EXE_adamite"))
        .arg("scan")
        .arg(&case.image)
        .arg("--output")
        .arg(&path)
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout)?;
    assert_eq!(text.lines().count(), 1);
    assert!(
        text.starts_with(&format!("saved {} (", path.display())),
        "{text}"
    );
    assert!(text.ends_with(" KiB)\n"), "{text}");
    case.assert_no_temporary_files()?;
    let output = case.save(&path, &["--force", "--bytes"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout)?,
        format!(
            "saved {} ({})\n",
            path.display(),
            fs::metadata(&path)?.len()
        )
    );
    case.assert_no_temporary_files()?;
    Ok(())
}

#[test]
fn existing_output_is_preserved_until_force_replaces_it() -> TestResult {
    let case = Case::new("save-existing", "basic-4k")?;
    let path = case.directory.join("saved.adm");
    let original = b"existing output must survive";
    fs::write(&path, original)?;
    let output = case.save(&path, &["--json"]);
    assert_eq!(
        output.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr)?;
    assert!(stderr.contains("already exists"), "{stderr}");
    assert!(!stderr.contains("scanning "), "{stderr}");
    assert_eq!(fs::read(&path)?, original);
    case.assert_no_temporary_files()?;

    let output = case.save(&path, &["--force", "--json"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    ScanOutput::from_bytes(&fs::read(&path)?)?;
    assert_eq!(
        without_source_and_time(case.json("info", &path)),
        without_source_and_time(case.json("info", &case.image))
    );
    case.assert_no_temporary_files()?;
    Ok(())
}

#[test]
fn existing_drive_output_is_rejected_before_scanning() -> TestResult {
    let case = Case::new("save-drive-existing", "basic-4k")?;
    let path = case.directory.join("saved.adm");
    let original = b"existing drive output";
    fs::write(&path, original)?;
    let output = Command::new(env!("CARGO_BIN_EXE_adamite"))
        .arg("scan")
        .arg("C:")
        .arg("-o")
        .arg(&path)
        .arg("--json")
        .output()?;
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr)?;
    assert!(stderr.contains("already exists"), "{stderr}");
    assert!(!stderr.contains("scanning "), "{stderr}");
    assert_eq!(fs::read(&path)?, original);
    case.assert_no_temporary_files()?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn dangling_output_symlink_is_rejected_before_scanning() -> TestResult {
    use std::os::unix::fs::symlink;
    let case = Case::new("save-dangling-output", "basic-4k")?;
    let path = case.directory.join("saved.adm");
    let missing = case.directory.join("missing.adm");
    symlink(&missing, &path)?;
    let output = case.save(&path, &["--json"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr)?;
    assert!(stderr.contains("already exists"), "{stderr}");
    assert!(!stderr.contains("scanning "), "{stderr}");
    assert_eq!(fs::read_link(&path)?, missing);
    assert!(!missing.exists());
    case.assert_no_temporary_files()?;
    Ok(())
}

#[test]
fn same_as_source_is_rejected_before_scanning() -> TestResult {
    let case = Case::new("save-source", "basic-4k")?;
    let original = fs::read(&case.image)?;
    for path in [&case.image, &case.directory.join(".").join("source.img")] {
        for flags in [vec!["--json"], vec!["--force", "--json"]] {
            let output = case.save(path, &flags);
            assert_eq!(output.status.code(), Some(1));
            assert!(output.stdout.is_empty());
            let stderr = String::from_utf8(output.stderr)?;
            let expected = if flags.contains(&"--force") {
                "resolves to the source image"
            } else {
                "already exists"
            };
            assert!(stderr.contains(expected), "{stderr}");
            assert!(!stderr.contains("scanning "), "{stderr}");
            assert_eq!(fs::read(&case.image)?, original);
            case.assert_no_temporary_files()?;
        }
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn output_symlink_to_source_is_rejected_before_scanning() -> TestResult {
    use std::os::unix::fs::symlink;
    let case = Case::new("save-source-symlink", "basic-4k")?;
    let original = fs::read(&case.image)?;
    let path = case.directory.join("alias.img");
    symlink(&case.image, &path)?;
    let output = case.save(&path, &["--force", "--json"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr)?;
    assert!(stderr.contains("resolves to the source image"), "{stderr}");
    assert!(!stderr.contains("scanning "), "{stderr}");
    assert_eq!(fs::read(&case.image)?, original);
    assert!(fs::symlink_metadata(&path)?.file_type().is_symlink());
    case.assert_no_temporary_files()?;
    Ok(())
}

#[test]
fn directory_and_temporary_creation_failures_leave_no_temporary_files() -> TestResult {
    let case = Case::new("save-io-failures", "basic-4k")?;
    let directory = case.directory.join("directory.adm");
    fs::create_dir(&directory)?;
    let sentinel = directory.join("keep.txt");
    fs::write(&sentinel, b"keep")?;
    for flags in [vec!["--json"], vec!["--force", "--json"]] {
        let output = case.save(&directory, &flags);
        assert_eq!(
            output.status.code(),
            Some(1),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8(output.stderr)?;
        assert!(
            stderr.contains(&format!("output {} is a directory", directory.display())),
            "{stderr}"
        );
        assert!(!stderr.contains("scanning "), "{stderr}");
        assert_eq!(fs::read(&sentinel)?, b"keep");
        case.assert_no_temporary_files()?;
    }
    let output = case.save(
        &case.directory.join("missing").join("saved.adm"),
        &["--force", "--json"],
    );
    assert_eq!(
        output.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8(output.stderr)?.contains("caused by:"));
    assert_eq!(fs::read(&sentinel)?, b"keep");
    case.assert_no_temporary_files()?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn output_symlink_to_directory_is_rejected_before_scanning() -> TestResult {
    use std::os::unix::fs::symlink;
    let case = Case::new("save-directory-symlink", "basic-4k")?;
    let directory = case.directory.join("directory");
    fs::create_dir(&directory)?;
    let sentinel = directory.join("keep.txt");
    fs::write(&sentinel, b"keep")?;
    let path = case.directory.join("alias.adm");
    symlink(&directory, &path)?;
    for flags in [vec!["--json"], vec!["--force", "--json"]] {
        let output = case.save(&path, &flags);
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8(output.stderr)?;
        assert!(
            stderr.contains(&format!("output {} is a directory", path.display())),
            "{stderr}"
        );
        assert!(!stderr.contains("scanning "), "{stderr}");
        assert_eq!(fs::read(&sentinel)?, b"keep");
        assert_eq!(fs::read_link(&path)?, directory);
        case.assert_no_temporary_files()?;
    }
    Ok(())
}

#[test]
fn concurrent_saves_publish_once_without_clobbering() -> TestResult {
    let case = Case::new("save-concurrent", "basic-4k")?;
    let path = case.directory.join("saved.adm");
    let mut command = Command::new(env!("CARGO_BIN_EXE_adamite"));
    command
        .arg("scan")
        .arg(&case.image)
        .arg("-o")
        .arg(&path)
        .arg("--json")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let first = command.spawn()?;
    let second = command.spawn()?;
    let mut published = 0;
    for output in [first.wait_with_output()?, second.wait_with_output()?] {
        match output.status.code() {
            Some(0) => {
                published += 1;
                let value: Value = serde_json::from_slice(&output.stdout)?;
                assert_eq!(value["bytes"], fs::metadata(&path)?.len());
            }
            Some(1) => {
                assert!(output.stdout.is_empty());
                assert!(String::from_utf8(output.stderr)?.contains("already exists"));
            }
            code => panic!(
                "unexpected save exit {code:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            ),
        }
    }
    assert_eq!(published, 1, "exactly one save must publish");
    ScanOutput::from_bytes(&fs::read(&path)?)?;
    case.assert_no_temporary_files()?;
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
        "  Allocated size           2.9 MiB\n",
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
        "  Allocated size           3039232\n",
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
        vec!["info", "unused", "-o", "out.adm"],
        vec!["scan", "unused", "--force"],
        vec!["info"],
        vec!["--json"],
        vec!["browse", "--json"],
        vec!["browse", "unused", "--json"],
        vec!["browse", "unused", "--bytes"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_adamite"))
            .args(&args)
            .output()?;
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8(output.stderr)?;
        assert!(stderr.starts_with("error:"));
        if args.contains(&"--force") {
            assert!(stderr.contains("--output <FILE>"), "{stderr}");
            assert!(!stderr.contains("scanning "), "{stderr}");
        }
    }
    for args in [
        vec![],
        vec!["--help"],
        vec!["info", "--help"],
        vec!["scan", "--help"],
        vec!["browse", "--help"],
        vec!["--version"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_adamite"))
            .args(&args)
            .output()?;
        assert!(output.status.success(), "{args:?}");
        assert!(output.stderr.is_empty());
        let text = String::from_utf8(output.stdout)?;
        if !args.contains(&"--version") {
            assert!(text.contains("--trace-chrome <FILE>"), "{text}");
        }
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
fn browse_requires_terminal_stdout_before_loading_or_scanning() -> TestResult {
    let case = Case::new("browse-pipe", "basic-4k")?;
    let saved = case.snapshot("saved.adm")?;
    for source in [&case.image, &saved, &case.directory.join("missing.img")] {
        let output = case.command("browse", source, &[]);
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8(output.stderr)?;
        assert!(
            stderr.contains("requires standard output to be a terminal"),
            "{stderr}"
        );
        assert!(!stderr.contains("scanning"), "{stderr}");
        assert!(
            !stderr.contains("caused by:"),
            "source must not be opened: {stderr}"
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

#[cfg(unix)]
#[test]
fn unwritable_output_exits_four_when_permissions_are_enforced() -> TestResult {
    use std::{io, os::unix::fs::PermissionsExt};
    let case = Case::new("save-permission-failure", "basic-4k")?;
    let permissions = fs::metadata(&case.directory)?.permissions();
    fs::set_permissions(&case.directory, fs::Permissions::from_mode(0o500))?;
    let path = case.directory.join("saved.adm");
    let probe = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path);
    let unwritable =
        matches!(&probe, Err(error) if error.kind() == io::ErrorKind::PermissionDenied);
    drop(probe);
    let output = unwritable.then(|| case.save(&path, &["--json"]));
    fs::set_permissions(&case.directory, permissions)?;
    if let Some(output) = output {
        assert_eq!(
            output.status.code(),
            Some(4),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8(output.stderr)?.contains("create temporary snapshot"));
        assert!(!path.exists());
    }
    case.assert_no_temporary_files()?;
    Ok(())
}

#[test]
fn chrome_trace_contains_scan_stages_batches_and_heap() -> TestResult {
    let case = Case::new("chrome-trace-scan", "basic-4k")?;
    let path = case.directory.join("scan.json");
    let output = Command::new(env!("CARGO_BIN_EXE_adamite"))
        .arg("--trace-chrome")
        .arg(&path)
        .arg("scan")
        .arg(&case.image)
        .output()?;
    assert!(output.status.success(), "{output:?}");
    let trace: Value = serde_json::from_slice(&fs::read(&path)?)?;
    let events = trace.as_array().ok_or("trace is not an array")?;
    for name in [
        "source_detect",
        "scan",
        "bootstrap",
        "ingest",
        "merge",
        "link",
        "build",
        "aggregate",
        "finalize",
        "driver_batch",
        "reader_read",
        "render",
    ] {
        assert!(
            events.iter().any(|event| event["name"] == name),
            "missing {name}"
        );
        let heap = events
            .iter()
            .find(|event| event["name"] == "heap_usage" && event["args"]["span_name"] == name)
            .ok_or_else(|| format!("missing heap event for {name}"))?;
        for field in ["heap_current", "heap_peak", "heap_delta", "process_peak"] {
            heap["args"][field]
                .as_str()
                .ok_or("missing heap argument")?
                .parse::<i128>()?;
        }
    }
    assert_eq!(
        events
            .iter()
            .filter(|event| event["name"] == "bootstrap" && event["ph"] == "b")
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event["name"] == "bootstrap" && event["ph"] == "e")
            .count(),
        1
    );
    let batch = events
        .iter()
        .find(|event| event["name"] == "driver_batch" && event["ph"] == "e")
        .ok_or("missing completed batch")?;
    assert!(
        batch["args"]["bytes"]
            .as_str()
            .ok_or("missing bytes")?
            .parse::<usize>()?
            > 0
    );
    assert!(
        batch["args"]["requests"]
            .as_str()
            .ok_or("missing requests")?
            .parse::<usize>()?
            > 0
    );
    Ok(())
}

#[test]
fn chrome_trace_is_global_and_finishes_on_command_errors() -> TestResult {
    let case = Case::new("chrome-trace-errors", "basic-4k")?;
    let saved = case.snapshot("source.adm")?;
    for command in ["scan", "info", "top", "ext", "ls", "browse"] {
        let path = case.directory.join(format!("{command}.json"));
        let source = if command == "scan" {
            &case.image
        } else {
            &saved
        };
        let output = Command::new(env!("CARGO_BIN_EXE_adamite"))
            .arg(command)
            .arg(source)
            .arg("--trace-chrome")
            .arg(&path)
            .output()?;
        assert_eq!(
            output.status.code(),
            Some(if command == "browse" { 2 } else { 0 }),
            "{output:?}"
        );
        let trace: Value = serde_json::from_slice(&fs::read(path)?)?;
        assert!(trace.is_array());
    }
    let path = case.directory.join("failure.json");
    let output = Command::new(env!("CARGO_BIN_EXE_adamite"))
        .arg("--trace-chrome")
        .arg(&path)
        .arg("info")
        .arg(case.directory.join("missing.img"))
        .output()?;
    assert_eq!(output.status.code(), Some(1));
    let trace: Value = serde_json::from_slice(&fs::read(path)?)?;
    assert!(
        trace
            .as_array()
            .ok_or("trace is not an array")?
            .iter()
            .any(|event| {
                event["name"] == "heap_usage" && event["args"]["span_name"] == "source_detect"
            })
    );
    Ok(())
}

#[test]
fn chrome_trace_covers_snapshot_saving_loading_and_validation() -> TestResult {
    let case = Case::new("chrome-trace-persistence", "basic-4k")?;
    let saved = case.directory.join("saved.adm");
    let trace_path = case.directory.join("save.json");
    let output = Command::new(env!("CARGO_BIN_EXE_adamite"))
        .arg("scan")
        .arg(&case.image)
        .arg("-o")
        .arg(&saved)
        .arg("--force")
        .arg("--trace-chrome")
        .arg(&trace_path)
        .output()?;
    assert!(output.status.success(), "{output:?}");
    let bytes = fs::read(&saved)?;
    assert_eq!(ScanOutput::from_bytes(&bytes)?.to_bytes()?, bytes);
    let trace: Value = serde_json::from_slice(&fs::read(trace_path)?)?;
    for name in ["snapshot_save", "persist_encode"] {
        assert!(
            trace
                .as_array()
                .ok_or("trace is not an array")?
                .iter()
                .any(|event| { event["name"] == name && event["ph"] == "e" }),
            "missing {name}"
        );
    }
    let trace_path = case.directory.join("load.json");
    let output = Command::new(env!("CARGO_BIN_EXE_adamite"))
        .arg("info")
        .arg(&saved)
        .arg("--trace-chrome")
        .arg(&trace_path)
        .output()?;
    assert!(output.status.success(), "{output:?}");
    let trace: Value = serde_json::from_slice(&fs::read(trace_path)?)?;
    for name in ["snapshot_load", "persist_decode", "persist_validate"] {
        assert!(
            trace
                .as_array()
                .ok_or("trace is not an array")?
                .iter()
                .any(|event| { event["name"] == name && event["ph"] == "e" }),
            "missing {name}"
        );
    }
    Ok(())
}

#[test]
fn invalid_resource_limits_exit_two_before_opening_source() -> TestResult {
    for (flag, value) in [
        ("--profile", "fast"),
        ("--max-read-rate", "0"),
        ("--max-read-rate", "unlimited"),
        ("--max-read-rate", "999999999999999999999G"),
        ("--threads", "0"),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_adamite"))
            .args(["scan", "missing-source.img", flag, value])
            .output()?;
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8(output.stderr)?.contains("error:"));
    }
    Ok(())
}

#[test]
fn snapshot_queries_ignore_scan_limits() -> TestResult {
    let case = Case::new("snapshot-resource-limits", "basic-4k")?;
    let snapshot = case.snapshot("source.adm")?;
    for name in ["info", "top", "ext", "ls"] {
        let expected = case.json(name, &snapshot);
        let actual = case.json_with(
            name,
            &snapshot,
            &[
                "--profile",
                "background",
                "--max-read-rate",
                "1",
                "--threads",
                "8",
            ],
        );
        assert_eq!(actual, expected);
    }
    Ok(())
}

#[test]
fn status_file_records_ordered_progress_and_saved_outcome() -> TestResult {
    let case = Case::new("status-success", "basic-4k")?;
    let destination = case.directory.join("saved.adm");
    let status = case.directory.join("scan.jsonl");
    let output = Command::new(env!("CARGO_BIN_EXE_adamite"))
        .arg("scan")
        .arg(&case.image)
        .arg("-o")
        .arg(&destination)
        .arg("--status-file")
        .arg(&status)
        .arg("--json")
        .output()?;
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stderr)?,
        format!("scanning {}…\n", case.image.display())
    );
    let report: Value = serde_json::from_slice(&output.stdout)?;
    let lines: Vec<Value> = fs::read_to_string(status)?
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;
    let (last, progress) = lines.split_last().ok_or("empty status file")?;
    assert_eq!(
        last,
        &json!({"done": {"saved": destination.to_string_lossy(), "bytes": fs::metadata(&destination)?.len()}})
    );
    assert_eq!(last["done"]["bytes"], report["bytes"]);
    assert!(!progress.is_empty());
    let phases = [
        "boot",
        "record0",
        "discover",
        "stream",
        "merge",
        "link",
        "snapshot",
        "aggregate",
        "finalize",
        "done",
    ];
    let mut previous_phase = 0;
    let mut previous_records = 0;
    let mut previous_bytes = 0;
    for line in progress {
        let value = line.get("progress").ok_or("unexpected terminal status")?;
        let phase = phases
            .iter()
            .position(|phase| value["phase"] == *phase)
            .ok_or("unknown phase")?;
        let records = value["records"].as_u64().ok_or("missing records")?;
        let bytes = value["bytes"].as_u64().ok_or("missing bytes")?;
        assert!(phase >= previous_phase, "{line}");
        assert!(records >= previous_records, "{line}");
        assert!(bytes >= previous_bytes, "{line}");
        previous_phase = phase;
        previous_records = records;
        previous_bytes = bytes;
    }
    Ok(())
}

#[test]
fn status_file_failure_matches_cli_message_and_exit_code() -> TestResult {
    let case = Case::new("status-failure", "basic-4k")?;
    fs::write(&case.image, [0; 512])?;
    let status = case.directory.join("scan.jsonl");
    let destination = case.directory.join("saved.adm");
    let output = Command::new(env!("CARGO_BIN_EXE_adamite"))
        .arg("scan")
        .arg(&case.image)
        .arg("-o")
        .arg(&destination)
        .arg("--status-file")
        .arg(&status)
        .output()?;
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr)?;
    let message = stderr
        .split_once("error: ")
        .ok_or("missing error line")?
        .1
        .lines()
        .take_while(|line| !line.starts_with("hint: "))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(message.contains("\ncaused by: "), "{message}");
    let lines: Vec<Value> = fs::read_to_string(status)?
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.get("failed").is_some())
            .count(),
        1
    );
    assert_eq!(
        lines.last(),
        Some(&json!({"failed": {"message": message, "exit_code": output.status.code()}}))
    );
    assert!(lines.iter().all(|line| line.get("done").is_none()));
    assert!(!destination.exists());
    Ok(())
}

#[test]
fn status_file_requires_output_and_preserves_existing_file() -> TestResult {
    let case = Case::new("status-usage", "basic-4k")?;
    let status = case.directory.join("scan.jsonl");
    let output = Command::new(env!("CARGO_BIN_EXE_adamite"))
        .arg("scan")
        .arg(&case.image)
        .arg("--status-file")
        .arg(&status)
        .output()?;
    assert_eq!(output.status.code(), Some(2));
    assert!(!status.exists());
    fs::write(&status, "keep existing status\n")?;
    let destination = case.directory.join("saved.adm");
    let output = Command::new(env!("CARGO_BIN_EXE_adamite"))
        .arg("scan")
        .arg(&case.image)
        .arg("-o")
        .arg(&destination)
        .arg("--status-file")
        .arg(&status)
        .output()?;
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(fs::read_to_string(status)?, "keep existing status\n");
    assert!(!String::from_utf8(output.stderr)?.contains("scanning "));
    assert!(!destination.exists());
    Ok(())
}

#[test]
fn no_elevate_preserves_every_command_on_images_and_snapshots() -> TestResult {
    let case = Case::new("no-elevate", "basic-4k")?;
    let snapshot = case.snapshot("source.adm")?;
    for source in [&case.image, &snapshot] {
        for name in ["scan", "info", "top", "ext", "ls", "browse"] {
            if name == "browse" || (name == "scan" && source == &snapshot) {
                let expected = case.command(name, source, &[]);
                let actual = case.command(name, source, &["--no-elevate"]);
                assert_eq!(actual.status.code(), expected.status.code(), "{name}");
                assert_eq!(actual.stdout, expected.stdout, "{name}");
                assert_eq!(actual.stderr, expected.stderr, "{name}");
            } else {
                let expected = case.json(name, source);
                let actual = case.json_with(name, source, &["--no-elevate"]);
                if source == &snapshot {
                    assert_eq!(actual, expected, "{name}");
                } else {
                    assert_eq!(
                        without_source_and_time(actual),
                        without_source_and_time(expected),
                        "{name}"
                    );
                }
            }
        }
    }
    let destination = case.directory.join("saved.adm");
    let saved = case.save(&destination, &["--no-elevate", "--json"]);
    assert!(saved.status.success(), "{saved:?}");
    assert!(destination.exists());
    Ok(())
}

#[test]
fn status_file_reports_done_when_saved_output_pipe_is_closed() -> TestResult {
    let case = Case::new("status-broken-pipe", "basic-4k")?;
    let destination = case.directory.join("saved.adm");
    let status = case.directory.join("scan.jsonl");
    let mut child = Command::new(env!("CARGO_BIN_EXE_adamite"))
        .arg("scan")
        .arg(&case.image)
        .arg("-o")
        .arg(&destination)
        .arg("--status-file")
        .arg(&status)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    drop(child.stdout.take());
    let output = child.wait_with_output()?;
    assert!(output.status.success(), "{output:?}");
    assert!(!String::from_utf8(output.stderr)?.contains("error:"));
    let lines: Vec<Value> = fs::read_to_string(status)?
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;
    assert_eq!(
        lines.last(),
        Some(
            &json!({"done": {"saved": destination.to_string_lossy(), "bytes": fs::metadata(destination)?.len()}})
        )
    );
    assert!(lines.iter().all(|line| line.get("failed").is_none()));
    Ok(())
}

fn query_body(
    case: &Case,
    command: &str,
    source: &Path,
    flags: &[&str],
) -> Result<String, Box<dyn Error>> {
    let output = case.command(command, source, flags);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout)?;
    Ok(text.split_once('\n').ok_or("report header")?.1.to_owned())
}

fn expected_top_item(snapshot: &Snapshot, id: NodeId) -> Value {
    let node = snapshot.node(id).expect("ranked file");
    json!({ "path": snapshot.path(id).expect("path").expect("existing path"),
        "size": node.size(), "alloc": node.alloc(), "mtime_filetime": node.mtime().to_string() })
}

fn expected_search_item(snapshot: &Snapshot, id: NodeId) -> Value {
    let mut item = expected_ls_item(snapshot, id);
    item.as_object_mut().expect("item object").insert(
        "path".to_owned(),
        json!(snapshot.path(id).expect("path").expect("existing path")),
    );
    item
}

#[test]
fn filtered_top_queries_match_core_on_images_and_snapshots() -> TestResult {
    let case = Case::new("filtered-top", "basic-4k")?;
    let saved = case.snapshot("filtered.adm")?;
    let snapshot = &case.output.snapshot;
    let documents =
        snapshot.rank_files(NodeId::ROOT, &FileFilter::Kind(FileKind::Document), 100)?;
    assert!(
        documents
            .iter()
            .any(|id| !snapshot.top_files().contains(id)),
        "fixture must exercise a result beyond the stored hundred"
    );
    let compressed = case.path_id("/compressed");
    let selected = format!("/#{}", compressed.raw());
    for source in [&case.image, &saved] {
        for (flags, under, filter) in [
            (
                vec!["--ext", ".TxT", "--n", "100"],
                NodeId::ROOT,
                FileFilter::extensions(["txt"])?,
            ),
            (
                vec!["--kind", "document", "--n", "100"],
                NodeId::ROOT,
                FileFilter::Kind(FileKind::Document),
            ),
            (
                vec!["--under", "/compressed", "--n", "100"],
                compressed,
                FileFilter::All,
            ),
            (
                vec!["--under", &selected, "--ext", ".TXT", "--n", "100"],
                compressed,
                FileFilter::extensions(["txt"])?,
            ),
            (
                vec!["--ext", "", "--ext", ".txt,BIN", "--n", "100"],
                NodeId::ROOT,
                FileFilter::extensions(["", "txt", "bin"])?,
            ),
            (
                vec!["--under", "/other", "--n", "100"],
                case.path_id("/other"),
                FileFilter::All,
            ),
        ] {
            let value = case.json_with("top", source, &flags);
            let expected: Vec<_> = snapshot
                .rank_files(under, &filter, 100)?
                .into_iter()
                .map(|id| expected_top_item(snapshot, id))
                .collect();
            assert_eq!(value["items"], json!(expected), "{flags:?}");
            assert_eq!(value["command"], "top");
            assert_eq!(value.as_object().ok_or("top report")?.len(), 5);
            let text = query_body(&case, "top", source, &flags)?;
            for item in &expected {
                assert!(
                    text.contains(item["path"].as_str().ok_or("path")?),
                    "{text}"
                );
            }
        }
        assert!(
            query_body(&case, "top", source, &["--ext", "txt", "--n", "100"])?
                .contains("/tiny.txt")
        );
        assert_eq!(
            query_body(&case, "top", source, &["--ext", ".txt", "--n", "100"])?,
            query_body(&case, "top", source, &["--kind", "document", "--n", "100"])?
        );
        assert_eq!(
            query_body(&case, "top", source, &["--n", "100"])?,
            query_body(&case, "top", source, &["--under", "/", "--n", "100"])?
        );
        for kind in FileKind::ALL {
            let value = case.json_with(
                "top",
                source,
                &["--kind", kind.name(), "--under", "/compressed"],
            );
            assert_eq!(
                value["items"].as_array().ok_or("items")?.len(),
                usize::from(kind == FileKind::Document)
            );
        }
        assert_eq!(
            query_body(&case, "top", source, &["--under", "/compressed"])?,
            concat!(
                "        Size        Alloc Path\n",
                "   256.0 KiB     32.0 KiB /compressed/c.txt\n",
            )
        );
        assert_eq!(
            query_body(
                &case,
                "top",
                source,
                &["--kind", "document", "--under", "/compressed", "--bytes"]
            )?,
            concat!(
                "                Size                Alloc Path\n",
                "              262144                32768 /compressed/c.txt\n",
            )
        );
    }
    Ok(())
}

#[test]
fn extension_and_kind_queries_match_core_and_keep_existing_fields() -> TestResult {
    let case = Case::new("scoped-extension-kind", "basic-4k")?;
    let saved = case.snapshot("groups.adm")?;
    let snapshot = &case.output.snapshot;
    for source in [&case.image, &saved] {
        for under in [None, Some("/"), Some("/compressed"), Some("/other")] {
            let mut flags = vec!["--n", "1000"];
            if let Some(path) = under {
                flags.extend(["--under", path]);
            }
            let id = under.map(|path| case.path_id(path)).unwrap_or(NodeId::ROOT);
            let extensions = case.json_with("ext", source, &flags);
            let expected: Vec<_> = snapshot.extension_stats_under(id)?.iter().map(|stat| json!({"extension": stat.extension(), "count": stat.count(), "total_size": stat.total_size(), "total_alloc": stat.total_alloc()})).collect();
            assert_eq!(extensions["items"], json!(expected));
            assert_eq!(extensions.as_object().ok_or("ext report")?.len(), 5);
            flags.extend(["--by", "kind"]);
            let kinds = case.json_with("ext", source, &flags);
            let expected: Vec<_> = snapshot.kind_stats(id)?.iter().map(|stat| json!({"kind": stat.kind().name(), "count": stat.count(), "total_size": stat.total_size(), "total_alloc": stat.total_alloc()})).collect();
            assert_eq!(kinds["items"], json!(expected));
            assert_eq!(kinds["command"], "ext");
            assert_eq!(kinds.as_object().ok_or("kind report")?.len(), 5);
            assert!(
                query_body(&case, "ext", source, &flags)?
                    .lines()
                    .next()
                    .ok_or("column names")?
                    .ends_with(" Kind")
            );
        }
        assert_eq!(
            query_body(&case, "ext", source, &[])?,
            query_body(&case, "ext", source, &["--by", "extension"])?
        );
        assert_eq!(
            query_body(&case, "ext", source, &["--under", "/compressed"])?,
            concat!(
                "        Size        Alloc      Count Extension\n",
                "   256.0 KiB     32.0 KiB          1 txt\n",
            )
        );
        assert_eq!(
            query_body(
                &case,
                "ext",
                source,
                &["--under", "/compressed", "--by", "kind", "--bytes"]
            )?,
            concat!(
                "                Size                Alloc      Count Kind\n",
                "              262144                32768          1 document\n",
            )
        );
        let kinds = case.json_with("ext", source, &["--by", "kind", "--n", "1"]);
        assert_eq!(kinds["items"].as_array().ok_or("items")?.len(), 1);
    }
    Ok(())
}

#[test]
fn search_json_and_text_pages_reach_the_end_in_core_order() -> TestResult {
    let case = Case::new("search-paging", "basic-4k")?;
    let saved = case.snapshot("search.adm")?;
    let snapshot = &case.output.snapshot;
    let all = snapshot.search(case.path_id("/bigdir"), &NamePattern::glob("f*")?, 0, 500)?;
    assert_eq!(all.total_matches(), 200);
    let expected: Vec<_> = all
        .items()
        .iter()
        .map(|&id| expected_search_item(snapshot, id))
        .collect();
    for source in [&case.image, &saved] {
        let mut combined = Vec::new();
        for offset in [0, 73, 146] {
            let offset_arg = offset.to_string();
            let flags = [
                "f*",
                "--under",
                "/BIGDIR",
                "--offset",
                &offset_arg,
                "--limit",
                "73",
            ];
            let value = case.json_with("search", source, &flags);
            assert_eq!(value["schema"], "adamite/1");
            assert_eq!(value["command"], "search");
            assert_eq!(value["units"], json!({"size": "bytes"}));
            assert_eq!(value.as_object().ok_or("search report")?.len(), 7);
            assert_eq!(value["total_matches"], 200);
            let items = value["items"].as_array().ok_or("search items")?;
            assert_eq!(
                items,
                &expected
                    .iter()
                    .skip(offset)
                    .take(73)
                    .cloned()
                    .collect::<Vec<_>>()
            );
            combined.extend(items.iter().cloned());
            let end = offset + items.len();
            let footer = if end < 200 {
                assert_eq!(value["next_offset"], end);
                format!("showing {}–{end} of 200 · next offset={end}", offset + 1)
            } else {
                assert!(value["next_offset"].is_null());
                format!("showing {}–{end} of 200", offset + 1)
            };
            let body = query_body(&case, "search", source, &flags)?;
            assert_eq!(body.lines().last().ok_or("footer")?, footer);
            assert_eq!(body.lines().count(), items.len() + 2);
            for item in items {
                assert!(body.contains(item["path"].as_str().ok_or("path")?));
            }
        }
        assert_eq!(combined, expected);
        for offset in [200, 1000, usize::MAX] {
            let arg = offset.to_string();
            let flags = ["f*", "--under", "/bigdir", "--offset", &arg];
            let value = case.json_with("search", source, &flags);
            assert_eq!(value["items"], json!([]));
            assert_eq!(value["total_matches"], 200);
            assert!(value["next_offset"].is_null());
            assert!(query_body(&case, "search", source, &flags)?.ends_with("showing 0 of 200\n"));
        }
        let limited = case.json_with("search", source, &["f*", "--under", "/bigdir"]);
        assert_eq!(
            limited["items"].as_array().ok_or("default items")?.len(),
            50
        );
        assert_eq!(limited["next_offset"], 50);
    }
    Ok(())
}

#[test]
fn search_rows_preserve_directory_markers_flags_and_byte_formatting() -> TestResult {
    let case = Case::new("search-rows", "basic-4k")?;
    let saved = case.snapshot("rows.adm")?;
    for source in [&case.image, &saved] {
        assert_eq!(
            query_body(&case, "search", source, &["bigdir"])?,
            concat!(
                "  Total size  Total alloc      Files Path\n",
                "         0 B     40.0 KiB        200 /bigdir/\n",
                "showing 1–1 of 1\n",
            )
        );
        assert_eq!(
            query_body(
                &case,
                "search",
                source,
                &["*.TXT", "--under", "/compressed", "--bytes"]
            )?,
            concat!(
                "          Total size          Total alloc      Files Path\n",
                "              262144                32768          1 /compressed/c.txt\n",
                "showing 1–1 of 1\n",
            )
        );
        for flags in [
            vec!["no-matching-name"],
            vec!["bigdir", "--under", "/bigdir"],
        ] {
            let value = case.json_with("search", source, &flags);
            assert_eq!(value["items"], json!([]));
            assert_eq!(value["total_matches"], 0);
            assert!(value["next_offset"].is_null());
            assert!(query_body(&case, "search", source, &flags)?.ends_with("no matches\n"));
        }
        let links = case.json_with("search", source, &["link"]);
        assert_eq!(links["total_matches"], 2);
        assert!(
            links["items"]
                .as_array()
                .ok_or("link items")?
                .iter()
                .any(|item| item["flags"]
                    .as_array()
                    .is_some_and(|flags| flags.contains(&json!("hardlink_dup"))))
        );
        let metadata = case.json_with("search", source, &["<ntfs metadata>"]);
        assert_eq!(metadata["total_matches"], 1);
        assert_eq!(metadata["items"][0]["kind"], "dir");
        assert!(
            metadata["items"][0]["flags"]
                .as_array()
                .ok_or("flags")?
                .contains(&json!("synthetic"))
        );
        let selected = format!("/#{}", case.path_id("/compressed").raw());
        let flags = [
            "*",
            "--under",
            &selected,
            "--profile",
            "max",
            "--threads",
            "4",
            "--no-elevate",
        ];
        assert_eq!(
            case.json_with("search", source, &flags)["items"][0]["path"],
            "/compressed/c.txt"
        );
    }
    let control = Case::from_image("search-control-escaping", &path_test_image())?;
    let body = query_body(&control, "search", &control.image, &["line*"])?;
    assert!(body.contains("/line\\u{a}\\u{9}\\u{1b}.TX\\u{a}T/"));
    assert!(!body.contains('\u{1b}'));
    Ok(())
}

#[test]
fn query_scopes_reuse_missing_file_and_ambiguous_path_errors() -> TestResult {
    let case = Case::new("query-scope-errors", "basic-4k")?;
    for (path, message) in [
        ("/missing", "not found"),
        ("/tiny.txt", "not a directory"),
        ("/tiny.txt/child", "not a directory"),
    ] {
        for (command, mut flags) in [("top", vec![]), ("ext", vec![]), ("search", vec!["*"])] {
            flags.extend(["--under", path, "--json"]);
            assert!(
                case.assert_command_failure(command, &flags, 1)
                    .contains(message)
            );
        }
    }
    let ambiguous = Case::from_image("query-scope-ambiguity", &path_test_image())?;
    for (command, mut flags) in [
        ("top", vec![]),
        ("ext", vec!["--by", "kind"]),
        ("search", vec!["*"]),
    ] {
        flags.extend(["--under", "/TWIN"]);
        assert!(
            ambiguous
                .assert_command_failure(command, &flags, 1)
                .contains("ambiguous path component")
        );
    }
    Ok(())
}

#[test]
fn query_filter_help_conflicts_ranges_and_glob_errors_are_usage_errors() -> TestResult {
    let case = Case::new("query-filter-errors", "basic-4k")?;
    let conflict = case.assert_command_failure("top", &["--ext", "txt", "--kind", "document"], 2);
    assert!(conflict.contains("cannot be used with"), "{conflict}");
    for flags in [
        vec!["--kind", "movie"],
        vec!["--kind", "Video"],
        vec!["--kind", "video", "--n", "101"],
    ] {
        case.assert_command_failure("top", &flags, 2);
    }
    case.assert_command_failure("ext", &["--by", "type"], 2);
    for limit in ["0", "501", "-1", "no", "65536"] {
        case.assert_command_failure("search", &["*", "--limit", limit], 2);
    }
    for offset in ["-1", "no", "18446744073709551616"] {
        case.assert_command_failure("search", &["*", "--offset", offset], 2);
    }
    for pattern in ["[".to_owned(), "{a,b".to_owned(), "x".repeat(257)] {
        let stderr = case.assert_command_failure("search", &[&pattern, "--json"], 2);
        assert!(stderr.contains("hint:"), "{stderr}");
        assert!(
            stderr.contains("glob") && stderr.contains("256"),
            "{stderr}"
        );
        assert!(stderr.contains("caused by:"), "{stderr}");
    }
    for command in ["top", "ext", "search"] {
        let output = Command::new(env!("CARGO_BIN_EXE_adamite"))
            .args([command, "--help"])
            .output()?;
        assert!(output.status.success());
        let text = String::from_utf8(output.stdout)?;
        assert!(text.contains("--under <PATH>"));
        if command == "top" {
            for kind in FileKind::ALL {
                assert!(text.contains(kind.name()));
            }
        }
    }
    assert_eq!(
        case.json_with("search", &case.image, &["*", "--limit", "500"])["next_offset"],
        Value::Null
    );
    Ok(())
}
