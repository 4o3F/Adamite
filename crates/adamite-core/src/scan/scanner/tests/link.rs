//! Link phase regressions.

use super::support::{TestResult, drive_to, ingest_volume, named_record};
use crate::error::ScanError;
use crate::scan::{ScanConfig, ScanPhase, ScanStep, Scanner};
use crate::snapshot::{NodeFlags, ParentTree};
use adamite_fixtures::Fixture;
use std::error::Error;
use std::path::Path;

fn name_bytes<'a>(
    tree: &'a ParentTree,
    node: &crate::snapshot::ResolvedNode,
) -> Result<&'a [u8], Box<dyn Error>> {
    let start = usize::try_from(node.name_off)?;
    let end = start
        .checked_add(usize::from(node.name_len))
        .ok_or("name range overflow")?;
    tree.names
        .get(start..end)
        .ok_or_else(|| "name pool range".into())
}

fn parent_path(tree: &ParentTree, mut id: usize) -> Result<String, Box<dyn Error>> {
    let mut parts = Vec::new();
    for _ in 0..tree.nodes.len() {
        if id == 0 {
            parts.reverse();
            return Ok(format!("/{}", parts.join("/")));
        }
        let node = tree.nodes.get(id).ok_or("parent-array index")?;
        parts.push(std::str::from_utf8(name_bytes(tree, node)?)?);
        id = node.parent;
    }
    Err("cycle in parent-array fixture path".into())
}

fn linking_fixture(name: &str) -> TestResult {
    let fixture = Fixture::load(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/ntfs")
            .join(format!("{name}.json")),
    )?;
    let mut scanner = Scanner::new(ScanConfig {
        max_outstanding: 4,
        chunk_size: 512,
        ..ScanConfig::default()
    })?;
    let (_, outcome) = drive_to(&mut scanner, fixture.image(), ScanPhase::Snapshot)?;
    outcome?;
    let tree = scanner
        .parent_tree
        .as_ref()
        .ok_or("flat parent-index array")?;
    let mut paths = std::collections::BTreeMap::new();
    for (id, node) in tree.nodes.iter().enumerate() {
        assert!(node.parent < tree.nodes.len());
        if id != 0 {
            assert!(
                !tree
                    .nodes
                    .get(node.parent)
                    .ok_or("parent")?
                    .flags
                    .contains(NodeFlags::HARDLINK_DUP),
                "children attached to secondary directory"
            );
        }
        paths.insert(parent_path(tree, id)?, id);
    }
    for expected in &fixture.manifest().entries {
        let id = paths
            .get(&expected.path)
            .ok_or_else(|| format!("{name}: missing path {}", expected.path))?;
        let node = tree.nodes.get(*id).ok_or("manifest node")?;
        assert_eq!(
            node.file_ref
                .map(|reference| (reference.record(), reference.sequence())),
            Some((expected.record, expected.seq)),
            "{name}: {}",
            expected.path
        );
    }
    let metadata = *paths.get("/<NTFS metadata>").ok_or("metadata directory")?;
    let expected_meta: std::collections::BTreeSet<_> = fixture
        .manifest()
        .metafiles
        .iter()
        .map(|entry| entry.record)
        .collect();
    for expected in &expected_meta {
        let nodes: Vec<_> = tree
            .nodes
            .iter()
            .filter(|node| {
                node.file_ref
                    .is_some_and(|reference| reference.record() == *expected)
            })
            .collect();
        assert!(!nodes.is_empty(), "missing metadata record {expected}");
        for node in nodes {
            assert_eq!(
                node.parent, metadata,
                "metadata record {expected} outside metadata directory"
            );
        }
    }
    let stats = &scanner.ingest.as_ref().ok_or("stats")?.stats;
    assert_eq!(stats.metafiles, u64::try_from(expected_meta.len())?);
    assert_eq!(
        stats.hardlink_duplicates,
        u64::try_from(
            tree.nodes
                .iter()
                .filter(|node| node.flags.contains(NodeFlags::HARDLINK_DUP))
                .count()
        )?
    );
    if name == "basic-4k" {
        let links: Vec<_> = tree
            .nodes
            .iter()
            .filter(|node| {
                node.file_ref
                    .is_some_and(|reference| reference.record() == 270)
            })
            .collect();
        assert_eq!(links.len(), 3);
        assert_eq!(
            links
                .iter()
                .filter(|node| !node.flags.contains(NodeFlags::HARDLINK_DUP))
                .count(),
            1
        );
        assert!(!paths.contains_key("/deleted.txt"));
        assert!(
            tree.nodes
                .iter()
                .all(|node| name_bytes(tree, node).is_ok_and(|name| name != b"LONGFI~1.TXT"))
        );
        let deepest = fixture
            .manifest()
            .entries
            .iter()
            .max_by_key(|entry| entry.path.matches('/').count())
            .ok_or("deep chain oracle")?;
        assert!(deepest.path.matches('/').count() >= 41);
        assert!(paths.contains_key(&deepest.path));
    }
    if name == "frag-512" {
        for fill in &fixture.manifest().fill {
            let directory = *paths.get(&fill.dir).ok_or("filler directory path")?;
            assert_eq!(
                tree.nodes
                    .iter()
                    .filter(|node| node.parent == directory)
                    .count(),
                fill.count,
                "{} child count",
                fill.dir
            );
        }
    }
    let (_, outcome) = drive_to(&mut scanner, fixture.image(), ScanPhase::Aggregate)?;
    outcome?;
    let (_, outcome) = drive_to(&mut scanner, fixture.image(), ScanPhase::Done)?;
    outcome?;
    assert!(matches!(scanner.step()?, ScanStep::Done(_)));
    assert!(matches!(scanner.step(), Err(ScanError::Inactive)));
    Ok(())
}

#[test]
fn linking_fixture_basic_4k_paths_hardlinks_aliases_and_deep_chain() -> TestResult {
    linking_fixture("basic-4k")
}

#[test]
fn linking_fixture_frag_512_paths_and_filler_child_counts() -> TestResult {
    linking_fixture("frag-512")
}

#[test]
fn linking_fixture_native_4kn_64k_paths_and_metadata() -> TestResult {
    linking_fixture("4kn-64k")
}

#[test]
fn linking_driver_counts_invalid_utf16_at_conversion_once() -> TestResult {
    let volume = ingest_volume(
        16,
        vec![(13, named_record(13, vec![0xd800, 65, 0xdc00]).build())],
    )?;
    let mut scanner = Scanner::new(ScanConfig::default())?;
    let (_, outcome) = drive_to(&mut scanner, &volume, ScanPhase::Merge)?;
    outcome?;
    assert_eq!(
        scanner
            .ingest
            .as_ref()
            .ok_or("pre-link stats")?
            .stats
            .invalid_utf16,
        0
    );
    let (_, outcome) = drive_to(&mut scanner, &volume, ScanPhase::Snapshot)?;
    outcome?;
    assert_eq!(
        scanner
            .ingest
            .as_ref()
            .ok_or("post-link stats")?
            .stats
            .invalid_utf16,
        2
    );
    Ok(())
}
