//! Arena, name pool and CSR behavior through the frozen read interface.

use super::*;
use crate::ScanContext;
use crate::{
    ScanStats,
    entry::{EntryName, EntryRecord},
    tree::link::{Linker, ParentTree, ResolvedNode},
};
use adamite_fixtures::builders::BootSectorBuilder;
use adamite_ntfs::FileRef;
use std::error::Error;

type TestResult = Result<(), Box<dyn Error>>;

fn entry(record: u64, parent: u64, name: &str, directory: bool) -> EntryRecord {
    EntryRecord {
        file_ref: FileRef::from_raw((7u64 << 48) | record),
        attrs: 0x20,
        mtime: 123,
        size: 17,
        ads_size: 3,
        alloc: 512,
        names: vec![EntryName {
            parent: FileRef::from_raw((7u64 << 48) | parent),
            namespace: 1,
            name: name.encode_utf16().collect(),
        }],
        nlink: 2,
        reparse_tag: Some(0xa000_000c),
        flags: if directory {
            NodeFlags::DIRECTORY
        } else {
            NodeFlags::default()
        },
    }
}
fn root() -> EntryRecord {
    entry(5, 5, ".", true)
}
fn link(entries: Vec<EntryRecord>) -> Result<ParentTree, Box<dyn Error>> {
    let mut linker = Linker::new(entries)?;
    let mut stats = ScanStats::default();
    for _ in 0..100_000 {
        if linker.step(&mut stats)? {
            return Ok(linker.result);
        }
    }
    Err("linker did not terminate".into())
}
fn build(entries: Vec<EntryRecord>, context: &ScanContext) -> Result<Snapshot, Box<dyn Error>> {
    let tree = link(entries)?;
    let geometry = adamite_ntfs::parse_boot_sector(&BootSectorBuilder::default().build())?;
    let mut builder = SnapshotBuilder::new(tree, geometry, context)?;
    for _ in 0..100_000 {
        if let Some(snapshot) = builder.step()? {
            return Ok(snapshot);
        }
    }
    Err("snapshot builder did not terminate".into())
}

#[test]
fn snapshot_root_cold_columns_and_volume_context() -> TestResult {
    let context = ScanContext {
        label: Some("volume".into()),
        scanned_at_filetime: Some(123),
        free_bytes: Some(4096),
    };
    let snapshot = build(vec![root()], &context)?;
    assert_eq!(snapshot.len(), 1);
    assert!(!snapshot.is_empty());
    assert_eq!(snapshot.name(NodeId::ROOT), Some("/"));
    assert_eq!(snapshot.path(NodeId::ROOT)?, Some("/".into()));
    assert_eq!(snapshot.children(NodeId::ROOT), Some([].as_slice()));
    let node = snapshot.node(NodeId::ROOT).ok_or("root")?;
    assert_eq!(node.id(), NodeId::ROOT);
    assert_eq!(node.parent(), NodeId::ROOT);
    assert_eq!((node.size(), node.ads_size(), node.alloc()), (17, 3, 512));
    assert_eq!((node.attrs(), node.mtime(), node.nlink()), (0x20, 123, 2));
    assert_eq!(node.file_ref(), Some(FileRef::from_raw((7u64 << 48) | 5)));
    assert_eq!(node.reparse_tag(), Some(0xa000_000c));
    assert!(node.flags().contains(NodeFlags::DIRECTORY));
    assert_eq!(
        (node.total_size(), node.total_alloc(), node.file_count()),
        (0, 0, 0)
    );
    assert_eq!(snapshot.volume().context(), &context);
    assert_eq!(
        snapshot.volume().geometry(),
        adamite_ntfs::parse_boot_sector(&BootSectorBuilder::default().build())?
    );
    assert_eq!(std::mem::size_of::<Node>(), 64);
    let shared = std::sync::Arc::new(snapshot);
    assert_eq!(shared.clone().name(NodeId::ROOT), Some("/"));
    Ok(())
}

#[test]
fn snapshot_invalid_ids_return_absence() -> TestResult {
    let snapshot = build(vec![root()], &ScanContext::default())?;
    for id in [NodeId::from_raw(1), NodeId::from_raw(u32::MAX)] {
        assert_eq!(snapshot.node(id), None);
        assert_eq!(snapshot.name(id), None);
        assert_eq!(snapshot.children(id), None);
        assert_eq!(snapshot.path(id)?, None);
    }
    assert_eq!(snapshot.volume().context(), &ScanContext::default());
    Ok(())
}

#[test]
fn snapshot_unicode_names_share_one_exact_pool() -> TestResult {
    fn assert_copy<T: Copy>() {}
    assert_copy::<ResolvedNode>();
    let mut invalid = entry(65, 5, "", false);
    invalid.names.first_mut().ok_or("name")?.name = vec![0xd800, 65, 0xdc00];
    let snapshot = build(
        vec![entry(64, 5, "文😀", false), root(), invalid],
        &ScanContext::default(),
    )?;
    assert_eq!(snapshot.path(NodeId::from_raw(1))?, Some("/文😀".into()));
    assert_eq!(snapshot.path(NodeId::from_raw(2))?, Some("/�A�".into()));
    let total = snapshot.data().nodes.iter().try_fold(0usize, |sum, node| {
        sum.checked_add(usize::from(node.name_len))
            .ok_or("pool sum")
    })?;
    assert_eq!(snapshot.data().names.len(), total);
    let mut end = 0usize;
    for node in &snapshot.data().nodes {
        assert_eq!(usize::try_from(node.name_off)?, end);
        end = end
            .checked_add(usize::from(node.name_len))
            .ok_or("pool cursor")?;
    }
    Ok(())
}

#[test]
fn snapshot_ten_thousand_deep_path_is_iterative() -> TestResult {
    let mut entries = vec![root()];
    let mut parent = 5;
    for record in 64..10_064 {
        entries.push(entry(record, parent, "d", true));
        parent = record;
    }
    let snapshot = build(entries, &ScanContext::default())?;
    let path = snapshot
        .path(NodeId::from_raw(10_000))?
        .ok_or("deep path")?;
    assert_eq!(path, "/d".repeat(10_000));
    Ok(())
}

#[test]
fn snapshot_representational_overflows_are_rejected() -> TestResult {
    let max = usize::try_from(u32::MAX)?;
    assert_eq!(checked_u32(max, "test")?, u32::MAX);
    let over = max
        .checked_add(1)
        .ok_or("host must represent u32 overflow")?;
    assert!(matches!(
        checked_u32(over, "node count"),
        Err(SnapshotError::Overflow {
            field: "node count"
        })
    ));
    assert!(matches!(
        name_range(max, 1),
        Err(SnapshotError::Overflow { .. })
    ));
    assert!(matches!(
        name_range(usize::MAX, 1),
        Err(SnapshotError::Overflow { .. })
    ));
    assert_eq!(name_range(0, usize::from(u16::MAX))?, (0, u16::MAX));
    assert!(matches!(
        name_range(0, usize::from(u16::MAX).checked_add(1).ok_or("name bound")?),
        Err(SnapshotError::Overflow {
            field: "name length"
        })
    ));
    let mut large = entry(64, 5, "", false);
    large.names.first_mut().ok_or("large name")?.name = vec![65; 65_536];
    let error = link(vec![root(), large])
        .err()
        .ok_or("overlong UTF-8 name accepted")?;
    assert!(matches!(
        error.downcast_ref::<crate::ScanError>(),
        Some(crate::ScanError::Snapshot {
            source: SnapshotError::Overflow {
                field: "name length"
            }
        })
    ));
    Ok(())
}

#[test]
fn snapshot_csr_ranges_partition_edges_and_sort_children_by_id() -> TestResult {
    let snapshot = build(
        vec![
            entry(66, 64, "leaf", false),
            entry(65, 5, "b", true),
            root(),
            entry(64, 5, "a", true),
        ],
        &ScanContext::default(),
    )?;
    let mut cursor = 0usize;
    let mut seen = std::collections::BTreeSet::new();
    for raw in 0..u32::try_from(snapshot.len())? {
        let id = NodeId::from_raw(raw);
        let node = snapshot.node(id).ok_or("node")?;
        assert_eq!(usize::try_from(node.child_start())?, cursor);
        let children = snapshot.children(id).ok_or("children")?;
        assert!(children.windows(2).all(|pair| pair.first() < pair.get(1)));
        cursor = cursor.checked_add(children.len()).ok_or("CSR cursor")?;
        for child in children {
            assert!(seen.insert(*child), "duplicate child {child:?}");
            assert_eq!(snapshot.node(*child).ok_or("child")?.parent(), id);
            assert_ne!(*child, NodeId::ROOT);
        }
    }
    assert_eq!(cursor, snapshot.data().children.len());
    assert_eq!(
        seen.len().checked_add(1).ok_or("node count")?,
        snapshot.len()
    );
    Ok(())
}

fn shuffled_entries() -> Vec<EntryRecord> {
    let mut multi = entry(64, 5, "z", true);
    multi.names.push(EntryName {
        parent: FileRef::from_raw((7u64 << 48) | 5),
        namespace: 1,
        name: vec![65],
    });
    vec![
        entry(65, 64, "child", false),
        entry(0, 5, "$MFT", false),
        root(),
        entry(66, 999, "orphan", true),
        multi,
    ]
}
#[test]
fn snapshot_deterministic_ids_under_shuffled_input() -> TestResult {
    let expected = build(shuffled_entries(), &ScanContext::default())?;
    assert_eq!(expected.name(NodeId::from_raw(1)), Some("<NTFS metadata>"));
    assert_eq!(expected.name(NodeId::from_raw(2)), Some("<orphaned>"));
    let mut expected_parent = 0;
    let mut names = Vec::new();
    for raw in 3..u32::try_from(expected.len())? {
        let view = expected.node(NodeId::from_raw(raw)).ok_or("ordered node")?;
        let record = view.file_ref().ok_or("ordinary reference")?.record();
        assert!(record >= expected_parent);
        expected_parent = record;
        names.push(expected.name(view.id()).ok_or("name")?);
    }
    assert_eq!(names, ["$MFT", "A", "z", "child", "orphan"]);
    for shift in 0..5 {
        let mut entries = shuffled_entries();
        entries.rotate_left(shift);
        if shift % 2 == 0 {
            entries.reverse();
        }
        for entry in &mut entries {
            entry.names.reverse();
        }
        assert_eq!(build(entries, &ScanContext::default())?, expected);
    }
    Ok(())
}
