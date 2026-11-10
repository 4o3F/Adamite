//! Linking behavior over synthetic EntryRecord lists.

use super::{EntryRecord, FileRef, Linker, NodeFlags, ParentTree, ScanError, ScanStats, Stage};
use crate::{graph::entry::EntryName, snapshot::SnapshotError};
use std::error::Error;

type TestResult = Result<(), Box<dyn Error>>;

#[test]
fn linking_bounded_push_preserves_limits_and_precheck_errors() -> TestResult {
    let limit = usize::try_from(u32::MAX)?;
    let mut items = vec![(); limit.checked_sub(1).ok_or("invalid node limit")?];
    Linker::push_bounded(&mut items, ())?;
    assert_eq!(items.len(), limit);
    let error = Linker::push_bounded(&mut items, ())
        .err()
        .ok_or("link storage bound was ignored")?;
    if limit < usize::MAX {
        assert!(matches!(
            error,
            ScanError::Snapshot {
                source: SnapshotError::Overflow {
                    field: "link storage length"
                }
            }
        ));
    } else {
        assert!(matches!(
            error,
            ScanError::Overflow {
                operation: "tree linking"
            }
        ));
    }
    assert_eq!(items.len(), limit);

    let mut items = vec![(); usize::MAX];
    assert!(matches!(
        Linker::push_bounded(&mut items, ()),
        Err(ScanError::Overflow {
            operation: "tree linking"
        })
    ));
    assert_eq!(items.len(), usize::MAX);
    Ok(())
}

#[test]
fn linking_counter_overflow_keeps_tree_context() {
    let mut stats = ScanStats {
        invalid_utf16: u64::MAX,
        ..ScanStats::default()
    };
    let mut pool = Vec::new();
    assert!(matches!(
        Linker::append_name(&mut pool, [0xd800].into_iter(), &mut stats),
        Err(ScanError::Overflow {
            operation: "tree linking"
        })
    ));
    assert_eq!(stats.invalid_utf16, u64::MAX);
    assert!(pool.is_empty());
}
fn reference(record: u64) -> FileRef {
    FileRef::from_raw((7u64 << 48) | record)
}
fn name(parent: u64, name: &str) -> EntryName {
    EntryName {
        parent: reference(parent),
        namespace: 1,
        name: name.encode_utf16().collect(),
    }
}
fn entry(record: u64, parent: u64, label: &str, directory: bool) -> EntryRecord {
    let flags = if directory {
        NodeFlags::DIRECTORY
    } else {
        NodeFlags::default()
    };
    EntryRecord {
        file_ref: reference(record),
        attrs: if directory { 16 } else { 32 },
        mtime: 123,
        size: 100,
        ads_size: 3,
        alloc: 512,
        names: vec![name(parent, label)],
        nlink: 1,
        reparse_tag: None,
        flags,
    }
}
fn root() -> EntryRecord {
    entry(5, 5, ".", true)
}
// Compute cumulative work from the iterators, cursors and graph marks used by linking.
fn stage_number(stage: &Stage) -> usize {
    match stage {
        Stage::Prepare => 0,
        Stage::Metadata => 1,
        Stage::Resolve => 2,
        Stage::Cycles => 3,
        Stage::Convert => 4,
        Stage::Emit => 5,
        Stage::Done => 6,
    }
}

fn sum_work(items: impl IntoIterator<Item = usize>) -> Result<usize, Box<dyn Error>> {
    items.into_iter().try_fold(0usize, |total, item| {
        total
            .checked_add(item)
            .ok_or_else(|| "test progress overflow".into())
    })
}

fn observed_work(
    linker: &Linker,
    stats: &ScanStats,
    input_count: usize,
) -> Result<usize, Box<dyn Error>> {
    let stage = stage_number(&linker.stage);
    let cursor_work = |phase, total| {
        if stage < phase {
            0
        } else if stage == phase {
            linker.cursor
        } else {
            total
        }
    };
    let prepared = input_count
        .checked_sub(linker.entries.len())
        .ok_or("entry iterator moved backwards")?;
    let record_nodes = sum_work(linker.records.iter().map(|record| record.len))?;
    let metadata_nodes = usize::from(linker.metadata.is_some());
    let resolved_orphan = usize::from(stats.orphans > stats.cycle_nodes);
    let resolved = sum_work([record_nodes, metadata_nodes, resolved_orphan])?;
    let total_nodes = sum_work([
        record_nodes,
        metadata_nodes,
        usize::from(linker.orphaned.is_some()),
    ])?;
    let cycles = if stage < 3 {
        0
    } else {
        let marks = sum_work(linker.marks.iter().copied().map(usize::from))?;
        // Root and an orphan directory created during cycle repair start already finished.
        let automatic_orphan = linker.orphaned.is_some() && stats.orphans == stats.cycle_nodes;
        let automatic = if automatic_orphan { 4 } else { 2 };
        let visits_and_finishes = marks.checked_sub(automatic).ok_or("invalid graph marks")?;
        let scanned = if stage == 3 {
            linker.cursor
        } else {
            linker.marks.len()
        }
        .checked_sub(1)
        .ok_or("missing root mark")?;
        sum_work([
            visits_and_finishes,
            scanned,
            usize::try_from(stats.cycle_nodes)?,
        ])?
    };
    sum_work([
        prepared,
        cursor_work(1, linker.records.len()),
        cursor_work(2, resolved),
        cycles,
        cursor_work(4, total_nodes),
        linker.result.nodes.len(),
    ])
}

fn run(entries: Vec<EntryRecord>) -> Result<(ParentTree, ScanStats), Box<dyn Error>> {
    let limit = entries
        .len()
        .checked_mul(32)
        .and_then(|n| n.checked_add(256))
        .ok_or("link test step bound")?;
    let input_count = entries.len();
    let mut linker = Linker::new(entries)?;
    let mut stats = ScanStats::default();
    for _ in 0..limit {
        let before = observed_work(&linker, &stats, input_count)?;
        let done = linker.step(&mut stats)?;
        let after = observed_work(&linker, &stats, input_count)?;
        assert!(
            after
                .checked_sub(before)
                .ok_or("link progress moved backwards")?
                <= 256
        );
        if done {
            let before = stats.clone();
            assert!(linker.step(&mut stats)?);
            assert_eq!(
                stats, before,
                "finished linking must not repeat conversions or counters"
            );
            return Ok((linker.result, stats));
        }
    }
    Err("linker did not terminate".into())
}
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

fn ids(tree: &ParentTree, record: u64) -> Vec<usize> {
    tree.nodes
        .iter()
        .enumerate()
        .filter_map(|(id, node)| {
            node.file_ref
                .is_some_and(|reference| reference.record() == record)
                .then_some(id)
        })
        .collect()
}
fn primary(tree: &ParentTree, record: u64) -> Result<usize, Box<dyn Error>> {
    ids(tree, record)
        .into_iter()
        .find(|id| {
            tree.nodes
                .get(*id)
                .is_some_and(|node| !node.flags.contains(NodeFlags::HARDLINK_DUP))
        })
        .ok_or_else(|| format!("missing primary {record}").into())
}
fn group(tree: &ParentTree, label: &[u8]) -> Result<usize, Box<dyn Error>> {
    tree.nodes
        .iter()
        .position(|node| {
            name_bytes(tree, node).is_ok_and(|name| name == label) && node.file_ref.is_none()
        })
        .ok_or_else(|| "missing synthetic group".into())
}
fn path(tree: &ParentTree, mut id: usize) -> Result<String, Box<dyn Error>> {
    let mut pieces = Vec::new();
    for _ in 0..tree.nodes.len() {
        if id == 0 {
            pieces.reverse();
            return Ok(format!("/{}", pieces.join("/")));
        }
        let node = tree.nodes.get(id).ok_or("path parent index")?;
        pieces.push(std::str::from_utf8(name_bytes(tree, node)?)?);
        id = node.parent;
    }
    Err("cycle in parent-array path".into())
}

#[test]
fn linking_same_parent_hardlinks_keep_values_and_choose_raw_name_primary() -> TestResult {
    let mut file = entry(64, 5, "b", false);
    file.names.push(name(5, "a"));
    let (tree, stats) = run(vec![file, root()])?;
    let nodes = ids(&tree, 64);
    assert_eq!(nodes.len(), 2);
    assert_eq!(path(&tree, primary(&tree, 64)?)?, "/a");
    for id in nodes {
        let node = tree.nodes.get(id).ok_or("hardlink node")?;
        assert_eq!((node.size, node.ads_size, node.alloc), (100, 3, 512));
    }
    assert_eq!(stats.hardlink_duplicates, 1);
    assert!(
        tree.nodes.iter().all(|node| node.file_ref.is_some()),
        "unneeded synthetic directories"
    );
    assert_eq!(tree.nodes.first().ok_or("root")?.parent, 0);
    assert_eq!(name_bytes(&tree, tree.nodes.first().ok_or("root")?)?, b"/");
    Ok(())
}

#[test]
fn linking_cross_directory_primary_is_parent_then_raw_name() -> TestResult {
    let mut file = entry(64, 67, "a", false);
    file.names.push(name(66, "z"));
    let (tree, _) = run(vec![
        file,
        entry(67, 5, "right", true),
        root(),
        entry(66, 5, "left", true),
    ])?;
    assert_eq!(path(&tree, primary(&tree, 64)?)?, "/left/z");
    let mut unicode = entry(65, 5, "", false);
    unicode.names = vec![
        EntryName {
            parent: reference(5),
            namespace: 1,
            name: vec![0xe000],
        },
        EntryName {
            parent: reference(5),
            namespace: 1,
            name: vec![0xd800, 0xdc00],
        },
    ];
    let (tree, _) = run(vec![root(), unicode])?;
    assert_eq!(
        std::str::from_utf8(name_bytes(
            &tree,
            tree.nodes
                .get(primary(&tree, 65)?)
                .ok_or("unicode primary")?
        )?)?,
        "𐀀"
    );
    Ok(())
}

#[test]
fn linking_namespace_then_parent_sequence_break_primary_ties() -> TestResult {
    let mut file = entry(64, 5, "same", false);
    let mut stale = name(5, "same");
    stale.parent = FileRef::from_raw((8u64 << 48) | 5);
    stale.namespace = 0;
    file.names.push(stale);
    let (tree, stats) = run(vec![root(), file])?;
    let first = primary(&tree, 64)?;
    assert_eq!(
        tree.nodes.get(first).ok_or("primary")?.parent,
        group(&tree, b"<orphaned>")?
    );
    assert_eq!(stats.hardlink_duplicates, 1);
    assert_eq!(stats.orphans, 1);
    Ok(())
}

#[test]
fn linking_dos_suppression_dos_only_and_identical_link_deduplication() -> TestResult {
    let mut file = entry(64, 5, "LongFile.txt", false);
    let mut dos = name(5, "LONGFI~1.TXT");
    dos.namespace = 2;
    file.names.push(dos);
    let mut only = entry(65, 5, "ONLY~1.TXT", false);
    only.names.first_mut().ok_or("DOS name")?.namespace = 2;
    let mut duplicate = entry(66, 5, "same", false);
    let mut alias = name(5, "same");
    alias.namespace = 3;
    duplicate.names.extend([name(5, "same"), alias]);
    let (tree, stats) = run(vec![root(), file, only, duplicate])?;
    assert_eq!(ids(&tree, 64).len(), 1);
    assert!(
        tree.nodes
            .iter()
            .all(|node| name_bytes(&tree, node).is_ok_and(|name| name != b"LONGFI~1.TXT"))
    );
    assert_eq!(path(&tree, primary(&tree, 65)?)?, "/ONLY~1.TXT");
    assert_eq!(ids(&tree, 66).len(), 1);
    assert_eq!(stats.hardlink_duplicates, 0);
    Ok(())
}

#[test]
fn linking_stale_non_directory_and_missing_parents_become_orphans() -> TestResult {
    let mut stale = entry(64, 5, "stale", false);
    stale.names.first_mut().ok_or("stale name")?.parent = FileRef::from_raw((8u64 << 48) | 5);
    let (tree, stats) = run(vec![
        root(),
        stale,
        entry(65, 5, "file", false),
        entry(66, 65, "child", false),
        entry(67, 999, "missing", false),
    ])?;
    let orphaned = group(&tree, b"<orphaned>")?;
    for record in [64, 66, 67] {
        assert_eq!(
            tree.nodes
                .get(primary(&tree, record)?)
                .ok_or("orphan")?
                .parent,
            orphaned
        );
    }
    let synthetic = tree.nodes.get(orphaned).ok_or("orphan directory")?;
    assert_eq!(
        (synthetic.size, synthetic.ads_size, synthetic.alloc),
        (0, 0, 0)
    );
    assert!(synthetic.file_ref.is_none());
    assert!(synthetic.flags.contains(NodeFlags::DIRECTORY));
    assert!(synthetic.flags.contains(NodeFlags::SYNTHETIC));
    assert_eq!(stats.orphans, 3);
    assert_eq!(stats.cycle_nodes, 0);
    Ok(())
}

#[test]
fn linking_multiname_directory_children_use_only_primary() -> TestResult {
    let mut directory = entry(64, 5, "z", true);
    directory.names.push(name(5, "a"));
    let (tree, stats) = run(vec![root(), entry(65, 64, "kid", false), directory])?;
    assert_eq!(path(&tree, primary(&tree, 65)?)?, "/a/kid");
    let secondary = ids(&tree, 64)
        .into_iter()
        .find(|id| {
            tree.nodes
                .get(*id)
                .is_some_and(|node| node.flags.contains(NodeFlags::HARDLINK_DUP))
        })
        .ok_or("secondary directory")?;
    assert!(tree.nodes.iter().all(|node| node.parent != secondary));
    assert_eq!(stats.hardlink_duplicates, 1);
    Ok(())
}

#[test]
fn linking_nonroot_self_parent_is_repaired_without_losing_descendants() -> TestResult {
    let (tree, stats) = run(vec![
        root(),
        entry(64, 64, "self", true),
        entry(65, 64, "kid", false),
    ])?;
    assert_eq!(path(&tree, primary(&tree, 65)?)?, "/<orphaned>/self/kid");
    assert_eq!(stats.cycle_nodes, 1);
    assert_eq!(stats.orphans, 1);
    Ok(())
}

#[test]
fn linking_two_and_three_directory_cycles_reparent_every_member() -> TestResult {
    for cycle in [vec![(64, 65), (65, 64)], vec![(64, 65), (65, 66), (66, 64)]] {
        let mut entries = vec![root(), entry(100, 64, "descendant", false)];
        for &(record, parent) in &cycle {
            entries.push(entry(record, parent, &format!("d{record}"), true));
        }
        let (tree, stats) = run(entries)?;
        let orphaned = group(&tree, b"<orphaned>")?;
        for &(record, _) in &cycle {
            assert_eq!(
                tree.nodes
                    .get(primary(&tree, record)?)
                    .ok_or("cycle member")?
                    .parent,
                orphaned
            );
        }
        assert_eq!(stats.cycle_nodes, u64::try_from(cycle.len())?);
        assert_eq!(stats.orphans, stats.cycle_nodes);
        assert_eq!(
            path(&tree, primary(&tree, 100)?)?,
            "/<orphaned>/d64/descendant"
        );
    }
    Ok(())
}

#[test]
fn linking_metadata_groups_reserved_and_extend_children_once() -> TestResult {
    let mut metadata = entry(70, 11, "child", true);
    metadata.names.push(name(5, "other"));
    let (tree, stats) = run(vec![
        root(),
        entry(0, 5, "$MFT", false),
        entry(11, 5, "$Extend", true),
        metadata,
        entry(71, 70, "descendant", true),
        entry(72, 71, "leaf", false),
    ])?;
    let group = group(&tree, b"<NTFS metadata>")?;
    for record in [0, 11, 70] {
        for id in ids(&tree, record) {
            assert_eq!(tree.nodes.get(id).ok_or("metadata node")?.parent, group);
        }
    }
    assert_eq!(stats.metafiles, 3);
    assert_eq!(stats.hardlink_duplicates, 1);
    assert_eq!(
        path(&tree, primary(&tree, 72)?)?,
        "/<NTFS metadata>/other/descendant/leaf"
    );
    assert!(
        tree.nodes
            .iter()
            .all(|node| name_bytes(&tree, node).is_ok_and(|name| name != b"<orphaned>"))
    );
    Ok(())
}

#[test]
fn linking_missing_corrupt_or_duplicate_root_is_fatal() -> TestResult {
    for entries in [
        vec![entry(64, 5, "file", false)],
        vec![entry(5, 5, ".", false)],
        vec![root(), root()],
    ] {
        let error = run(entries).err().ok_or("expected root error")?;
        assert!(matches!(
            error.downcast_ref::<ScanError>(),
            Some(ScanError::RequiredRecord { record: 5 })
        ));
    }
    Ok(())
}

#[test]
fn linking_nameless_records_get_stable_labels_and_needed_groups() -> TestResult {
    let mut ordinary = entry(64, 5, "", false);
    ordinary.names.clear();
    let mut metadata = entry(12, 5, "", false);
    metadata.names.clear();
    let (tree, stats) = run(vec![root(), ordinary, metadata])?;
    assert_eq!(path(&tree, primary(&tree, 64)?)?, "/<orphaned>/<record 64>");
    assert_eq!(
        path(&tree, primary(&tree, 12)?)?,
        "/<NTFS metadata>/<record 12>"
    );
    assert_eq!((stats.orphans, stats.metafiles), (1, 1));
    Ok(())
}

#[test]
fn linking_ten_thousand_directory_chain_is_iterative_and_chunked() -> TestResult {
    let mut entries = vec![root()];
    for record in (64..10_064).rev() {
        entries.push(entry(
            record,
            if record == 64 {
                5
            } else {
                record.checked_sub(1).ok_or("chain parent")?
            },
            "d",
            true,
        ));
    }
    let (tree, stats) = run(entries)?;
    let path = path(&tree, primary(&tree, 10_063)?)?;
    assert_eq!(
        path.split('/').filter(|part| !part.is_empty()).count(),
        10_000
    );
    assert_eq!(tree.nodes.len(), 10_001);
    assert_eq!((stats.orphans, stats.cycle_nodes), (0, 0));
    Ok(())
}

#[test]
fn linking_invalid_surrogates_count_only_surviving_name_conversions() -> TestResult {
    let units = vec![0xd800, 65, 0xdc00];
    let mut bad = entry(64, 5, "", false);
    bad.names = vec![
        EntryName {
            parent: reference(5),
            namespace: 1,
            name: units.clone(),
        },
        EntryName {
            parent: reference(5),
            namespace: 3,
            name: units.clone(),
        },
    ];
    let mut suppressed = entry(65, 5, "valid", false);
    suppressed.names.push(EntryName {
        parent: reference(5),
        namespace: 2,
        name: units.clone(),
    });
    let (tree, stats) = run(vec![root(), bad, suppressed])?;
    let node = tree
        .nodes
        .get(primary(&tree, 64)?)
        .ok_or("invalid-name node")?;
    assert_eq!(
        std::str::from_utf8(name_bytes(&tree, node)?)?,
        String::from_utf16_lossy(&units)
    );
    assert_eq!(std::str::from_utf8(name_bytes(&tree, node)?)?, "�A�");
    assert_eq!(stats.invalid_utf16, 2);
    assert_eq!(stats.hardlink_duplicates, 0);
    Ok(())
}
