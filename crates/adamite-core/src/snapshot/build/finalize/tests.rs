//! Finalization work is measured from its real cursors and collections.

use super::*;
use crate::{
    scan::ScanContext,
    snapshot::{ParentTree, ResolvedNode, SnapshotBuilder},
};
use adamite_fixtures::builders::BootSectorBuilder;
use std::error::Error;

fn snapshot(files: u32) -> Result<Snapshot, Box<dyn Error>> {
    let mut tree = ParentTree::default();
    for raw in 0..=files {
        let name = if raw == 0 {
            "/".to_owned()
        } else {
            format!("file.{raw}")
        };
        let offset = u32::try_from(tree.names.len())?;
        tree.names.extend_from_slice(name.as_bytes());
        tree.nodes.push(ResolvedNode {
            parent: 0,
            file_ref: None,
            original_parent: None,
            name_off: offset,
            name_len: u16::try_from(name.len())?,
            flags: if raw == 0 {
                NodeFlags::DIRECTORY
            } else {
                NodeFlags::default()
            },
            attrs: 0,
            mtime: 0,
            size: 1,
            ads_size: 0,
            alloc: 1,
            nlink: 1,
            reparse_tag: None,
        });
    }
    let geometry = adamite_ntfs::boot::VolumeGeometry::from_boot_sector(
        &BootSectorBuilder::default().build(),
    )?;
    let mut builder = SnapshotBuilder::new(tree, geometry, &ScanContext::default())?;
    for _ in 0..1000 {
        if let Some(snapshot) = builder.step()? {
            return Ok(snapshot);
        }
    }
    Err("snapshot construction did not terminate".into())
}

fn observed_work(
    finalizer: &Finalizer,
    snapshot: &Snapshot,
    groups: usize,
) -> Result<usize, Box<dyn Error>> {
    let nodes = snapshot.len();
    let sorted = if matches!(finalizer.stage, Stage::SortChildren) {
        finalizer.cursor
    } else {
        nodes
    };
    let collected = match finalizer.stage {
        Stage::SortChildren => 0,
        Stage::Collect => finalizer.cursor,
        _ => nodes,
    };
    let pushed = match finalizer.stage {
        Stage::SortChildren | Stage::Collect => 0,
        Stage::HeapBuild => groups
            .checked_sub(finalizer.pending.len())
            .ok_or("pending group count")?,
        _ => groups,
    };
    let emitted = snapshot.ext_stats().len();
    [sorted, collected, pushed, emitted]
        .into_iter()
        .try_fold(0usize, |sum, value| {
            sum.checked_add(value).ok_or_else(|| "work overflow".into())
        })
}

#[test]
fn aggregation_finalization_steps_process_at_most_256_items() -> Result<(), Box<dyn Error>> {
    let mut snapshot = snapshot(600)?;
    let mut finalizer = Finalizer::new();
    for _ in 0..100 {
        let before = observed_work(&finalizer, &snapshot, 600)?;
        let done = finalizer.step(&mut snapshot)?;
        let after = observed_work(&finalizer, &snapshot, 600)?;
        assert!(after.checked_sub(before).ok_or("work moved backwards")? <= 256);
        if done {
            assert_eq!(snapshot.ext_stats().len(), 600);
            assert_eq!(snapshot.top_files().len(), 100);
            let before = snapshot.clone();
            assert!(finalizer.step(&mut snapshot)?);
            assert_eq!(snapshot, before);
            return Ok(());
        }
    }
    Err("finalizer did not terminate".into())
}

#[test]
fn aggregation_finalization_yields_after_one_directory_sort() -> Result<(), Box<dyn Error>> {
    let mut snapshot = snapshot(3)?;
    for node in &mut snapshot.data_mut().nodes {
        node.flags = NodeFlags::DIRECTORY.raw();
    }
    let mut finalizer = Finalizer::new();
    for expected in 1..=snapshot.len() {
        assert!(!finalizer.step(&mut snapshot)?);
        assert!(matches!(finalizer.stage, Stage::SortChildren));
        assert_eq!(finalizer.cursor, expected, "one directory sort per call");
    }
    assert!(finalizer.step(&mut snapshot)?);
    Ok(())
}
