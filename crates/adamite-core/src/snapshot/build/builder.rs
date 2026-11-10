//! Bounded conversion of a parent-index array into an arena and CSR children.

use super::input::{NameRange, ParentTree, ResolvedNode};
use crate::{
    ScanContext,
    snapshot::{Node, NodeId, Snapshot, SnapshotError, VolumeInfo, storage::SnapshotStorage},
    util::VecExt,
};
use adamite_ntfs::boot::VolumeGeometry;
use std::vec::IntoIter;

/// Maximum nodes or edges processed in one driver call.
const ITEMS_PER_STEP: usize = 256;

#[derive(Debug)]
enum Stage {
    Nodes,
    Counts,
    Prefix,
    Children,
    Done,
}

#[derive(Debug)]
pub(crate) struct SnapshotBuilder {
    source: IntoIter<ResolvedNode>,
    snapshot: Option<Snapshot>,
    stage: Stage,
    cursor: usize,
    child_start: usize,
    cursors: Vec<u32>,
}

impl SnapshotBuilder {
    pub(crate) fn new(
        tree: ParentTree,
        geometry: VolumeGeometry,
        context: &ScanContext,
    ) -> Result<Self, SnapshotError> {
        let len = tree.nodes.len();
        u32::try_from(len).map_err(|_| SnapshotError::overflow("node count"))?;
        u32::try_from(tree.names.len()).map_err(|_| SnapshotError::overflow("name pool length"))?;
        if len == 0 {
            return Err(SnapshotError::invalid("missing root"));
        }
        let label = context
            .label
            .as_ref()
            .map(|label| {
                let mut owned = String::new();
                owned.try_reserve_exact(label.len())?;
                owned.push_str(label);
                Ok::<_, SnapshotError>(owned)
            })
            .transpose()?;
        let mut snapshot = SnapshotStorage {
            top_files: Vec::new(),
            ext_stats: Vec::new(),
            nodes: Vec::new(),
            names: tree.names,
            children: Vec::new(),
            ads_size: Vec::new(),
            file_refs: Vec::new(),
            attrs: Vec::new(),
            nlink: Vec::new(),
            reparse_tags: Vec::new(),
            volume: VolumeInfo {
                geometry,
                context: ScanContext {
                    label,
                    scanned_at_filetime: context.scanned_at_filetime,
                    free_bytes: context.free_bytes,
                },
            },
        };
        VecExt::try_reserve_exact(&mut snapshot.nodes, len)?;
        VecExt::try_reserve_exact(&mut snapshot.ads_size, len)?;
        VecExt::try_reserve_exact(&mut snapshot.file_refs, len)?;
        VecExt::try_reserve_exact(&mut snapshot.attrs, len)?;
        VecExt::try_reserve_exact(&mut snapshot.nlink, len)?;
        VecExt::try_reserve_exact(&mut snapshot.reparse_tags, len)?;
        let child_len = len
            .checked_sub(1)
            .ok_or_else(|| SnapshotError::invalid("missing root"))?;
        VecExt::try_reserve_exact(&mut snapshot.children, child_len)?;
        snapshot.children.resize(child_len, NodeId::ROOT);
        let mut cursors = Vec::new();
        VecExt::try_reserve_exact(&mut cursors, len)?;
        cursors.resize(len, 0);
        Ok(Self {
            source: tree.nodes.into_iter(),
            snapshot: Some(Snapshot::from_storage(snapshot)?),
            stage: Stage::Nodes,
            cursor: 0,
            child_start: 0,
            cursors,
        })
    }

    pub(crate) fn step(&mut self) -> Result<Option<Snapshot>, SnapshotError> {
        for _ in 0..ITEMS_PER_STEP {
            let Some(snapshot) = self.snapshot.as_mut() else {
                return Ok(None);
            };
            let snapshot = snapshot.data_mut();
            match self.stage {
                Stage::Nodes => {
                    if let Some(source) = self.source.next() {
                        let id = snapshot.nodes.len();
                        if source.parent >= self.cursors.len() || (id == 0 && source.parent != 0) {
                            return Err(SnapshotError::invalid("parent outside arena"));
                        }
                        let start = usize::try_from(source.name_off)
                            .map_err(|_| SnapshotError::overflow("host index"))?;
                        let (_, len) =
                            NameRange::new(start, usize::from(source.name_len))?.into_parts();
                        let end = start
                            .checked_add(usize::from(len))
                            .ok_or(SnapshotError::Overflow { field: "name end" })?;
                        let name = snapshot
                            .names
                            .get(start..end)
                            .and_then(|bytes| std::str::from_utf8(bytes).ok())
                            .ok_or_else(|| SnapshotError::invalid("invalid UTF-8 name range"))?;
                        if id == 0 && name != "/" {
                            return Err(SnapshotError::invalid("invalid root name"));
                        }
                        snapshot.nodes.push(Node {
                            size: source.size,
                            alloc: source.alloc,
                            total_size: 0,
                            total_alloc: 0,
                            mtime: source.mtime,
                            parent: NodeId::try_from_index(source.parent, "parent id")?.raw(),
                            child_start: 0,
                            child_len: 0,
                            file_count: 0,
                            name_off: source.name_off,
                            name_len: source.name_len,
                            flags: source.flags.raw(),
                        });
                        snapshot.ads_size.push(source.ads_size);
                        snapshot.file_refs.push(source.file_ref);
                        snapshot.attrs.push(source.attrs);
                        snapshot.nlink.push(source.nlink);
                        snapshot.reparse_tags.push(source.reparse_tag);
                    } else {
                        self.stage = Stage::Counts;
                        self.cursor = 1;
                    }
                }
                Stage::Counts => {
                    if let Some(node) = snapshot.nodes.get(self.cursor) {
                        let parent = usize::try_from(node.parent)
                            .map_err(|_| SnapshotError::overflow("host index"))?;
                        let node = snapshot
                            .nodes
                            .get_mut(parent)
                            .ok_or_else(|| SnapshotError::invalid("missing parent"))?;
                        node.child_len =
                            node.child_len
                                .checked_add(1)
                                .ok_or(SnapshotError::Overflow {
                                    field: "child count",
                                })?;
                        self.cursor =
                            self.cursor.checked_add(1).ok_or(SnapshotError::Overflow {
                                field: "child cursor",
                            })?;
                    } else {
                        self.stage = Stage::Prefix;
                        self.cursor = 0;
                    }
                }
                Stage::Prefix => {
                    if let Some(node) = snapshot.nodes.get_mut(self.cursor) {
                        node.child_start = u32::try_from(self.child_start)
                            .map_err(|_| SnapshotError::overflow("child range start"))?;
                        *self
                            .cursors
                            .get_mut(self.cursor)
                            .ok_or_else(|| SnapshotError::invalid("missing CSR cursor"))? =
                            node.child_start;
                        self.child_start = self
                            .child_start
                            .checked_add(
                                usize::try_from(node.child_len)
                                    .map_err(|_| SnapshotError::overflow("host index"))?,
                            )
                            .ok_or(SnapshotError::Overflow {
                                field: "child range end",
                            })?;
                        u32::try_from(self.child_start)
                            .map_err(|_| SnapshotError::overflow("child range end"))?;
                        self.cursor =
                            self.cursor.checked_add(1).ok_or(SnapshotError::Overflow {
                                field: "prefix cursor",
                            })?;
                    } else {
                        if self.child_start != snapshot.children.len() {
                            return Err(SnapshotError::invalid(
                                "CSR ranges do not partition edges",
                            ));
                        }
                        self.stage = Stage::Children;
                        self.cursor = 1;
                    }
                }
                Stage::Children => {
                    if let Some(node) = snapshot.nodes.get(self.cursor) {
                        let cursor = self
                            .cursors
                            .get_mut(
                                usize::try_from(node.parent)
                                    .map_err(|_| SnapshotError::overflow("host index"))?,
                            )
                            .ok_or_else(|| SnapshotError::invalid("missing parent CSR cursor"))?;
                        *snapshot
                            .children
                            .get_mut(
                                usize::try_from(*cursor)
                                    .map_err(|_| SnapshotError::overflow("host index"))?,
                            )
                            .ok_or_else(|| SnapshotError::invalid("child range outside CSR"))? =
                            NodeId::try_from_index(self.cursor, "child id")?;
                        *cursor = cursor.checked_add(1).ok_or(SnapshotError::Overflow {
                            field: "CSR cursor",
                        })?;
                        self.cursor =
                            self.cursor.checked_add(1).ok_or(SnapshotError::Overflow {
                                field: "child cursor",
                            })?;
                    } else {
                        self.stage = Stage::Done;
                        return Ok(self.snapshot.take());
                    }
                }
                Stage::Done => return Ok(None),
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NodeFlags;
    use adamite_fixtures::builders::BootSectorBuilder;
    use std::error::Error;

    fn observed_work(builder: &SnapshotBuilder, count: usize) -> Result<usize, Box<dyn Error>> {
        let edges = count.checked_sub(1).ok_or("root")?;
        let before = match builder.stage {
            Stage::Nodes => {
                return count
                    .checked_sub(builder.source.len())
                    .ok_or_else(|| "source moved backwards".into());
            }
            Stage::Counts => count,
            Stage::Prefix => count.checked_add(edges).ok_or("prefix work")?,
            Stage::Children | Stage::Done => count
                .checked_add(edges)
                .and_then(|n| n.checked_add(count))
                .ok_or("children work")?,
        };
        let current = match builder.stage {
            Stage::Counts | Stage::Children => {
                builder.cursor.checked_sub(1).ok_or("edge cursor")?
            }
            Stage::Prefix => builder.cursor,
            Stage::Done => edges,
            Stage::Nodes => return Err("unexpected node stage".into()),
        };
        before
            .checked_add(current)
            .ok_or_else(|| "work overflow".into())
    }

    #[test]
    fn snapshot_builder_steps_process_at_most_256_items() -> Result<(), Box<dyn Error>> {
        let count = 601;
        let mut nodes = vec![
            ResolvedNode {
                parent: 0,
                file_ref: None,
                original_parent: None,
                name_off: 1,
                name_len: 1,
                flags: NodeFlags::default(),
                attrs: 0,
                mtime: 0,
                size: 0,
                ads_size: 0,
                alloc: 0,
                nlink: 0,
                reparse_tag: None,
            };
            count
        ];
        let root = nodes.first_mut().ok_or("root")?;
        root.name_off = 0;
        root.flags = NodeFlags::DIRECTORY;
        let tree = ParentTree {
            nodes,
            names: b"/x".to_vec(),
        };
        let geometry = adamite_ntfs::boot::VolumeGeometry::from_boot_sector(
            &BootSectorBuilder::default().build(),
        )?;
        let mut builder = SnapshotBuilder::new(tree, geometry, &ScanContext::default())?;
        for _ in 0..100 {
            let before = observed_work(&builder, count)?;
            let result = builder.step()?;
            let after = observed_work(&builder, count)?;
            assert!(after.checked_sub(before).ok_or("work moved backwards")? <= 256);
            if let Some(snapshot) = result {
                assert_eq!(snapshot.len(), count);
                assert_eq!(
                    snapshot
                        .children(NodeId::ROOT)
                        .ok_or("root children")?
                        .len(),
                    count.checked_sub(1).ok_or("edge count")?
                );
                assert!(
                    builder.step()?.is_none(),
                    "snapshot must be handed off only once"
                );
                return Ok(());
            }
        }
        Err("builder did not terminate".into())
    }
}
