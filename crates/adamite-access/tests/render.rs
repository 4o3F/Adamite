//! Report construction and output failures through the public library API.

use adamite_access::{
    DirectoryLookup, ReportError,
    render::{Renderer, json::Json},
    report::{InfoReport, LsReport, SavedReport, TopReport},
};
use adamite_core::{scan::ScanConfig, snapshot::NodeId};
use adamite_fixtures::Fixture;
use adamite_io::{driver::SyncDriver, reader::SliceReader};
use std::{
    error::Error,
    io::{self, Write},
    path::Path,
};

type TestResult = Result<(), Box<dyn Error>>;

struct BrokenWriter;

impl Write for BrokenWriter {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::from(io::ErrorKind::BrokenPipe))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn reports_render_as_text_and_one_json_line_and_preserve_output_errors() -> TestResult {
    let fixture = Fixture::load(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/ntfs/basic-4k.json"),
    )?;
    let output =
        SyncDriver::new(ScanConfig::default())?.run(&mut SliceReader::new(fixture.image()))?;
    let path = Path::new("source.adm");
    let report = InfoReport::new("info", path, &output)?;
    let mut json = Vec::new();
    Renderer::new(true, false).write(&report, &mut json)?;
    assert_eq!(json.last(), Some(&b'\n'));
    assert_eq!(json.iter().filter(|&&byte| byte == b'\n').count(), 1);
    let value: serde_json::Value = serde_json::from_slice(&json)?;
    assert_eq!(value["schema"], "adamite/1");
    assert_eq!(value["command"], "info");
    assert_eq!(value["units"]["size"], "bytes");
    assert_eq!(value["stats"], serde_json::to_value(&report.stats)?);
    let saved = SavedReport::new(path, &output, Path::new("saved.adm"), 1536);
    for (raw_bytes, expected) in [
        (false, "saved saved.adm (1.5 KiB)\n"),
        (true, "saved saved.adm (1536)\n"),
    ] {
        let mut text = Vec::new();
        Renderer::new(false, raw_bytes).write(&saved, &mut text)?;
        assert_eq!(text, expected.as_bytes());
    }
    for json in [false, true] {
        let error = Renderer::new(json, false)
            .write(&report, &mut BrokenWriter)
            .expect_err("broken writer");
        assert_eq!(error.to_string(), "cannot write standard output");
        assert!(
            matches!(error, ReportError::Output { source } if source.kind() == io::ErrorKind::BrokenPipe)
        );
    }
    let error = Json(&report)
        .write(&mut BrokenWriter)
        .expect_err("broken JSON writer");
    assert!(
        matches!(error, ReportError::Output { source } if source.kind() == io::ErrorKind::BrokenPipe)
    );
    assert_eq!(output.snapshot.resolve_directory("/")?, NodeId::ROOT);
    let listing = LsReport::new(path, &output, "/", 0, 1)?;
    assert_eq!(
        listing.total_children,
        output
            .snapshot
            .children(NodeId::ROOT)
            .ok_or("root children")?
            .len()
    );
    assert_eq!(listing.items.len(), 1);
    let error = LsReport::new(path, &output, "/", usize::MAX, 1).expect_err("overflowing page");
    assert_eq!(
        error.to_string(),
        "--offset + --limit exceeds the supported range"
    );
    let error =
        TopReport::from_ids(path, &output, [NodeId::from_raw(u32::MAX)]).expect_err("missing node");
    assert_eq!(
        error.to_string(),
        "snapshot is inconsistent: missing top file"
    );
    Ok(())
}
