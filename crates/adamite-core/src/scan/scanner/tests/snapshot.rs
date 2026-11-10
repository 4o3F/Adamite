//! Snapshot phase regressions.

use super::support::{TestResult, drive_to};
use crate::error::ScanError;
use crate::scan::{ScanConfig, ScanPhase, ScanStep, Scanner};
use adamite_fixtures::Fixture;
use adamite_ntfs::record::reference::FileRef;
use std::path::Path;

fn snapshot_fixture(name: &str) -> TestResult {
    let fixture = Fixture::load(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/ntfs")
            .join(format!("{name}.json")),
    )?;
    let context = crate::scan::ScanContext {
        label: Some(name.into()),
        scanned_at_filetime: Some(123),
        free_bytes: Some(456),
    };
    let mut scanner = Scanner::new(ScanConfig {
        max_outstanding: 4,
        chunk_size: 512,
        context: context.clone(),
    })?;
    let (_, outcome) = drive_to(&mut scanner, fixture.image(), ScanPhase::Aggregate)?;
    outcome?;
    let output = scanner.output.as_ref().ok_or("snapshot output")?;
    let snapshot = &output.snapshot;
    assert_eq!(snapshot.volume().context(), &context);
    assert_eq!(&scanner.config.context, &context);
    let mut paths = std::collections::BTreeMap::new();
    for raw in 0..u32::try_from(snapshot.len())? {
        let id = crate::snapshot::NodeId::from_raw(raw);
        paths.insert(snapshot.path(id)?.ok_or("snapshot path")?, id);
    }
    for expected in &fixture.manifest().entries {
        let id = *paths
            .get(&expected.path)
            .ok_or_else(|| format!("{name}: missing {}", expected.path))?;
        let node = snapshot.node(id).ok_or("manifest node")?;
        assert_eq!(
            node.file_ref(),
            Some(FileRef::new(expected.record, expected.seq)?),
            "{name}: {}",
            expected.path
        );
        assert_eq!(node.attrs(), expected.attributes);
        assert_eq!(node.mtime(), expected.mtime_filetime);
        assert_eq!(u64::from(node.nlink()), expected.nlink);
        assert_eq!(node.reparse_tag(), expected.reparse_tag);
    }
    for expected in &fixture.manifest().metafiles {
        let leaf = expected
            .path
            .rsplit('/')
            .next()
            .ok_or("metafile basename")?;
        let path = format!("/<NTFS metadata>/{leaf}");
        let id = *paths
            .get(&path)
            .ok_or_else(|| format!("{name}: missing {path}"))?;
        assert_eq!(
            snapshot.node(id).ok_or("metafile node")?.file_ref(),
            Some(FileRef::new(expected.record, expected.seq)?)
        );
    }
    let (_, outcome) = drive_to(&mut scanner, fixture.image(), ScanPhase::Done)?;
    outcome?;
    assert!(matches!(scanner.step()?, ScanStep::Done(_)));
    assert!(matches!(scanner.step(), Err(ScanError::Inactive)));
    Ok(())
}

#[test]
fn snapshot_fixture_basic_4k_paths_and_file_refs() -> TestResult {
    snapshot_fixture("basic-4k")
}

#[test]
fn snapshot_fixture_frag_512_paths_and_file_refs() -> TestResult {
    snapshot_fixture("frag-512")
}

#[test]
fn snapshot_fixture_native_4kn_64k_paths_and_file_refs() -> TestResult {
    snapshot_fixture("4kn-64k")
}
