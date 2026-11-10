//! Aggregation and final ordering observed through the frozen snapshot API.

use super::*;
use crate::{
    ScanContext, ScanStats,
    entry::{EntryName, EntryRecord},
    snapshot::{Finalizer, SnapshotBuilder},
    tree::link::Linker,
};
use adamite_fixtures::builders::BootSectorBuilder;
use adamite_ntfs::record::reference::FileRef;
use std::error::Error;

type TestResult = Result<(), Box<dyn Error>>;
fn reference(record: u64) -> FileRef {
    FileRef::from_raw((7u64 << 48) | record)
}
fn file(record: u64, parent: u64, name: &str) -> EntryRecord {
    EntryRecord {
        file_ref: reference(record),
        attrs: 0,
        mtime: 0,
        size: 1,
        ads_size: 0,
        alloc: 10,
        names: vec![EntryName {
            parent: reference(parent),
            namespace: 1,
            name: name.encode_utf16().collect(),
        }],
        nlink: 1,
        reparse_tag: None,
        flags: NodeFlags::default(),
    }
}
fn directory(record: u64, parent: u64, name: &str) -> EntryRecord {
    let mut result = file(record, parent, name);
    result.flags = NodeFlags::DIRECTORY;
    result.size = 0;
    result.alloc = 0;
    result
}
fn root() -> EntryRecord {
    directory(5, 5, ".")
}
fn build(entries: Vec<EntryRecord>) -> Result<Snapshot, Box<dyn Error>> {
    let mut linker = Linker::new(entries)?;
    let mut stats = ScanStats::default();
    for _ in 0..100_000 {
        if linker.step(&mut stats)? {
            let geometry = adamite_ntfs::boot::VolumeGeometry::from_boot_sector(
                &BootSectorBuilder::default().build(),
            )?;
            let mut builder =
                SnapshotBuilder::new(linker.result, geometry, &ScanContext::default())?;
            for _ in 0..100_000 {
                if let Some(snapshot) = builder.step()? {
                    return Ok(snapshot);
                }
            }
            return Err("snapshot builder did not terminate".into());
        }
    }
    Err("linker did not terminate".into())
}
fn observed_work(aggregate: &Aggregator, count: usize) -> Result<usize, Box<dyn Error>> {
    let edges = match aggregate.stage {
        Stage::BreadthFirst => aggregate.order.len().checked_sub(1).ok_or("root")?,
        _ => count.checked_sub(1).ok_or("root")?,
    };
    let parents = if matches!(aggregate.stage, Stage::BreadthFirst) {
        aggregate.cursor
    } else {
        count
    };
    let initialized = match aggregate.stage {
        Stage::BreadthFirst => 0,
        Stage::Initialize => aggregate.cursor,
        _ => count,
    };
    let accumulated = match aggregate.stage {
        Stage::Accumulate => count
            .checked_sub(aggregate.cursor)
            .ok_or("reverse cursor")?,
        Stage::Done => count,
        _ => 0,
    };
    [edges, parents, initialized, accumulated]
        .into_iter()
        .try_fold(0usize, |sum, value| {
            sum.checked_add(value).ok_or_else(|| "work overflow".into())
        })
}
fn aggregate(snapshot: &mut Snapshot) -> Result<(), SnapshotError> {
    let mut aggregate = Aggregator::new(snapshot)?;
    for _ in 0..100_000 {
        let before = observed_work(&aggregate, snapshot.len())
            .map_err(|_| SnapshotError::invalid("test work calculation"))?;
        let done = aggregate.step(snapshot)?;
        let after = observed_work(&aggregate, snapshot.len())
            .map_err(|_| SnapshotError::invalid("test work calculation"))?;
        assert!(
            after
                .checked_sub(before)
                .ok_or_else(|| SnapshotError::invalid("work moved backwards"))?
                <= 256
        );
        if done {
            assert!(aggregate.step(snapshot)?);
            return Ok(());
        }
    }
    Err(SnapshotError::invalid("aggregator did not terminate"))
}
fn finish(entries: Vec<EntryRecord>) -> Result<Snapshot, Box<dyn Error>> {
    let mut snapshot = build(entries)?;
    aggregate(&mut snapshot)?;
    let mut finalize = Finalizer::new();
    for _ in 0..100_000 {
        if finalize.step(&mut snapshot)? {
            assert!(finalize.step(&mut snapshot)?);
            return Ok(snapshot);
        }
    }
    Err("finalization did not terminate".into())
}

#[test]
fn aggregation_directory_self_alloc_and_ads_accounting() -> TestResult {
    let mut root = root();
    root.alloc = 4;
    let mut dir = directory(64, 5, "dir");
    dir.alloc = 8;
    dir.size = 2;
    let mut leaf = file(65, 64, "leaf");
    leaf.size = 7;
    leaf.ads_size = 100;
    leaf.alloc = 16;
    let snapshot = finish(vec![leaf, dir, root])?;
    let root = snapshot.node(NodeId::ROOT).ok_or("root")?;
    assert_eq!(
        (root.total_size(), root.total_alloc(), root.file_count()),
        (9, 28, 1)
    );
    let dir = snapshot.node(NodeId::from_raw(1)).ok_or("directory")?;
    assert_eq!(
        (dir.size(), dir.alloc(), dir.total_size(), dir.total_alloc()),
        (2, 8, 9, 24)
    );
    let leaf = snapshot.node(NodeId::from_raw(2)).ok_or("file")?;
    assert_eq!(
        (leaf.size(), leaf.ads_size(), leaf.total_size()),
        (7, 100, 7)
    );
    let stat = snapshot.ext_stats().first().ok_or("extension")?;
    assert_eq!(
        (stat.count(), stat.total_size(), stat.total_alloc()),
        (1, 7, 16)
    );
    Ok(())
}

#[test]
fn aggregation_duplicate_links_keep_self_values_but_contribute_zero() -> TestResult {
    let mut linked = file(64, 5, "b");
    linked.size = 17;
    linked.ads_size = 3;
    linked.alloc = 512;
    linked.names.push(EntryName {
        parent: reference(5),
        namespace: 1,
        name: vec![65],
    });
    let snapshot = finish(vec![root(), linked])?;
    let root = snapshot.node(NodeId::ROOT).ok_or("root")?;
    assert_eq!(
        (root.total_size(), root.total_alloc(), root.file_count()),
        (17, 512, 1)
    );
    let duplicate = snapshot.node(NodeId::from_raw(2)).ok_or("duplicate")?;
    assert!(duplicate.flags().contains(NodeFlags::HARDLINK_DUP));
    assert_eq!(
        (duplicate.size(), duplicate.ads_size(), duplicate.alloc()),
        (17, 3, 512)
    );
    assert_eq!(
        (
            duplicate.total_size(),
            duplicate.total_alloc(),
            duplicate.file_count()
        ),
        (17, 512, 0)
    );
    assert_eq!(snapshot.top_files(), &[NodeId::from_raw(1)]);
    assert_eq!(snapshot.ext_stats().first().ok_or("extension")?.count(), 1);
    Ok(())
}

fn tie_entries() -> Vec<EntryRecord> {
    let mut lossy = file(68, 5, "");
    lossy.names = vec![
        EntryName {
            parent: reference(5),
            namespace: 1,
            name: vec![0xd801],
        },
        EntryName {
            parent: reference(5),
            namespace: 1,
            name: vec![0xd800],
        },
    ];
    vec![
        root(),
        file(64, 5, "𐀀"),
        file(65, 5, "\u{e000}"),
        file(66, 5, "same"),
        file(67, 5, "same"),
        lossy,
    ]
}
#[test]
fn aggregation_ties_use_utf16_file_ref_and_node_id() -> TestResult {
    let snapshot = finish(tie_entries())?;
    let expected: Vec<_> = [3, 4, 1, 2, 5, 6]
        .into_iter()
        .map(NodeId::from_raw)
        .collect();
    assert_eq!(snapshot.children(NodeId::ROOT).ok_or("children")?, expected);
    assert_eq!(
        snapshot.top_files(),
        &[
            NodeId::from_raw(3),
            NodeId::from_raw(4),
            NodeId::from_raw(1),
            NodeId::from_raw(2),
            NodeId::from_raw(5)
        ]
    );
    let mut shuffled = tie_entries();
    shuffled.reverse();
    assert_eq!(finish(shuffled)?, snapshot);
    Ok(())
}

#[test]
fn aggregation_extension_edge_cases_and_ascii_only_folding() -> TestResult {
    let mut entries = vec![root()];
    for (offset, name) in [
        ".bashrc",
        "a.",
        "noext",
        "A.TXT",
        "b.txt",
        "x.tar.gz",
        ".config.JSON",
        "a.ÄB",
        "b.Äb",
        "c.äB",
    ]
    .into_iter()
    .enumerate()
    {
        let mut entry = file(
            64u64.checked_add(u64::try_from(offset)?).ok_or("ordinal")?,
            5,
            name,
        );
        entry.size = 10;
        entry.ads_size = 100;
        entry.alloc = 512;
        entries.push(entry);
    }
    let snapshot = finish(entries)?;
    let actual: Vec<_> = snapshot
        .ext_stats()
        .iter()
        .map(|group| {
            (
                group.extension(),
                group.count(),
                group.total_size(),
                group.total_alloc(),
            )
        })
        .collect();
    assert_eq!(
        actual,
        [
            ("", 3, 30, 1536),
            ("txt", 2, 20, 1024),
            ("Äb", 2, 20, 1024),
            ("gz", 1, 10, 512),
            ("json", 1, 10, 512),
            ("äb", 1, 10, 512)
        ]
    );
    Ok(())
}

#[test]
fn aggregation_top_files_cap_order_and_metafiles() -> TestResult {
    let mut entries = vec![root()];
    for record in 64..214 {
        let mut entry = file(record, 5, &format!("f{record}"));
        entry.alloc = record;
        entries.push(entry);
    }
    let mut metadata = file(0, 5, "$MFT");
    metadata.alloc = 10_000;
    entries.push(metadata);
    let snapshot = finish(entries)?;
    assert_eq!(snapshot.top_files().len(), 100);
    let children = snapshot.children(NodeId::ROOT).ok_or("root children")?;
    let actual_children: Vec<_> = children
        .iter()
        .map(|id| {
            snapshot
                .node(*id)
                .and_then(|node| node.file_ref())
                .map(|reference| reference.record())
        })
        .collect();
    let expected_children: Vec<_> = std::iter::once(None)
        .chain((64..214).rev().map(Some))
        .collect();
    assert_eq!(
        actual_children, expected_children,
        "the metadata subtree ranks by its total allocation"
    );

    let expected: Vec<_> = std::iter::once(0).chain((64..214).rev().take(99)).collect();
    let actual: Vec<_> = snapshot
        .top_files()
        .iter()
        .map(|id| {
            snapshot
                .node(*id)
                .and_then(|node| node.file_ref())
                .map(|reference| reference.record())
        })
        .collect();
    assert_eq!(actual, expected.into_iter().map(Some).collect::<Vec<_>>());
    assert_eq!(snapshot.node(NodeId::ROOT).ok_or("root")?.file_count(), 151);
    Ok(())
}

#[test]
fn aggregation_ten_thousand_deep_chain_is_iterative_and_batched() -> TestResult {
    let mut entries = vec![root()];
    let mut parent = 5;
    for record in 64..10_064 {
        let mut entry = directory(record, parent, "d");
        entry.alloc = 1;
        if record == 10_063 {
            entry.flags = NodeFlags::default();
            entry.size = 7;
            entry.ads_size = 500;
            entry.alloc = 3;
        }
        entries.push(entry);
        parent = record;
    }
    let snapshot = finish(entries)?;
    let root = snapshot.node(NodeId::ROOT).ok_or("root")?;
    assert_eq!(
        (root.total_size(), root.total_alloc(), root.file_count()),
        (7, 10_002, 1)
    );
    assert_eq!(
        snapshot.path(NodeId::from_raw(10_000))?,
        Some("/d".repeat(10_000))
    );
    Ok(())
}

#[test]
fn aggregation_logical_and_allocation_overflow_return_errors() -> TestResult {
    for field in ["subtree logical bytes", "subtree allocation"] {
        let mut first = file(64, 5, "a");
        let mut second = file(65, 5, "b");
        first.size = 0;
        second.size = 0;
        first.alloc = 0;
        second.alloc = 0;
        if field == "subtree logical bytes" {
            first.size = u64::MAX;
            second.size = 1;
        } else {
            first.alloc = u64::MAX;
            second.alloc = 1;
        }
        let mut snapshot = build(vec![root(), first, second])?;
        assert!(
            matches!(aggregate(&mut snapshot), Err(SnapshotError::Overflow { field: actual }) if actual == field)
        );
    }
    Ok(())
}

#[test]
fn aggregation_bfs_handles_parents_with_larger_node_ids() -> TestResult {
    let mut leaf = file(64, 65, "leaf");
    leaf.size = 7;
    leaf.alloc = 8;
    let mut middle = directory(65, 66, "middle");
    middle.alloc = 2;
    let mut parent = directory(66, 5, "parent");
    parent.alloc = 3;
    let snapshot = finish(vec![leaf, middle, root(), parent])?;
    let root = snapshot.node(NodeId::ROOT).ok_or("root")?;
    assert_eq!(
        (root.total_size(), root.total_alloc(), root.file_count()),
        (7, 13, 1)
    );
    assert_eq!(
        snapshot.node(NodeId::from_raw(1)).ok_or("leaf")?.parent(),
        NodeId::from_raw(2)
    );
    assert_eq!(
        snapshot.node(NodeId::from_raw(2)).ok_or("middle")?.parent(),
        NodeId::from_raw(3)
    );
    Ok(())
}
