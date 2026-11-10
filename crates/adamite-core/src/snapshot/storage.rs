//! Frozen node arena, shared names and compressed sparse row child storage.

use super::{ExtensionStat, Node, NodeId, NodeView, SnapshotError};

use crate::scan::ScanContext;
use adamite_ntfs::{boot::VolumeGeometry, record::reference::FileRef};

/// Parsed volume geometry together with metadata supplied by the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeInfo {
    pub(super) geometry: VolumeGeometry,
    pub(super) context: ScanContext,
}

impl VolumeInfo {
    /// Returns the validated geometry, including the volume serial number.
    #[must_use]
    pub const fn geometry(&self) -> VolumeGeometry {
        self.geometry
    }
    /// Borrows optional label, scan timestamp and free bytes supplied by the caller.
    #[must_use]
    pub const fn context(&self) -> &ScanContext {
        &self.context
    }
}

/// Immutable arena with parallel cold columns, shared UTF-8 names and CSR children.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    storage: Box<[SnapshotStorage; 1]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SnapshotStorage {
    pub(super) nodes: Vec<Node>,
    pub(super) names: Vec<u8>,
    pub(super) children: Vec<NodeId>,
    pub(super) ads_size: Vec<u64>,
    pub(super) file_refs: Vec<Option<FileRef>>,
    pub(super) attrs: Vec<u32>,
    pub(super) nlink: Vec<u16>,
    pub(super) reparse_tags: Vec<Option<u32>>,
    pub(super) volume: VolumeInfo,
    pub(super) top_files: Vec<NodeId>,
    pub(super) ext_stats: Vec<ExtensionStat>,
}

impl Snapshot {
    pub(super) fn from_storage(storage: SnapshotStorage) -> Result<Self, SnapshotError> {
        // Indirection keeps ScanStep small; reserve the single storage allocation fallibly.
        let mut owned = Vec::new();
        owned.try_reserve_exact(1)?;
        owned.push(storage);
        let storage =
            owned
                .into_boxed_slice()
                .try_into()
                .map_err(|_| SnapshotError::InvalidTree {
                    reason: "invalid storage length",
                })?;
        Ok(Self { storage })
    }
    pub(super) fn data(&self) -> &SnapshotStorage {
        let [storage] = self.storage.as_ref();
        storage
    }
    pub(super) fn data_mut(&mut self) -> &mut SnapshotStorage {
        let [storage] = self.storage.as_mut();
        storage
    }

    /// Returns the number of nodes, including the root and synthetic directories.
    #[must_use]
    pub fn len(&self) -> usize {
        self.data().nodes.len()
    }
    /// Reports whether the arena contains no nodes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.data().nodes.is_empty()
    }
    /// Borrows the immutable volume geometry and caller metadata.
    #[must_use]
    pub fn volume(&self) -> &VolumeInfo {
        &self.data().volume
    }
    /// Borrows up to 100 primary files ordered by descending self allocation,
    /// then UTF-16 name, file reference and identifier; metafiles are included.
    #[must_use]
    pub fn top_files(&self) -> &[NodeId] {
        &self.data().top_files
    }
    /// Borrows primary-file extension summaries ordered by descending allocation,
    /// then ascending extension; ADS logical bytes are excluded.
    #[must_use]
    pub fn ext_stats(&self) -> &[ExtensionStat] {
        &self.data().ext_stats
    }
    /// Borrows the hot fields and cold metadata of an existing node.
    #[must_use]
    pub fn node(&self, id: NodeId) -> Option<NodeView<'_>> {
        let index = usize::try_from(id.raw()).ok()?;
        Some(NodeView {
            id,
            node: self.data().nodes.get(index)?,
            ads_size: *self.data().ads_size.get(index)?,
            file_ref: *self.data().file_refs.get(index)?,
            attrs: *self.data().attrs.get(index)?,
            nlink: *self.data().nlink.get(index)?,
            reparse_tag: *self.data().reparse_tags.get(index)?,
        })
    }
    /// Borrows children ordered by descending subtree allocation, then UTF-16 name,
    /// file reference and identifier; lossy names use their pooled replacement characters.
    #[must_use]
    pub fn children(&self, id: NodeId) -> Option<&[NodeId]> {
        let node = self.node(id)?;
        let start = usize::try_from(node.child_start()).ok()?;
        let len = usize::try_from(node.child_len()).ok()?;
        self.data().children.get(start..start.checked_add(len)?)
    }
    /// Borrows the node's lossy UTF-8 name directly from the shared pool.
    #[must_use]
    pub fn name(&self, id: NodeId) -> Option<&str> {
        let node = self.node(id)?;
        let start = usize::try_from(node.name_off()).ok()?;
        let end = start.checked_add(usize::from(node.name_len()))?;
        std::str::from_utf8(self.data().names.get(start..end)?).ok()
    }
    /// Builds a volume-relative path iteratively, returning None for an unknown identifier.
    ///
    /// # Errors
    ///
    /// Returns an allocation or length-overflow error, or InvalidTree if parent invariants fail.
    pub fn path(&self, id: NodeId) -> Result<Option<String>, SnapshotError> {
        if self.node(id).is_none() {
            return Ok(None);
        }
        let mut ancestors = Vec::new();
        let mut current = id;
        let mut len = 0usize;
        while current != NodeId::ROOT {
            if ancestors.len() >= self.data().nodes.len() {
                return Err(SnapshotError::InvalidTree {
                    reason: "parent cycle",
                });
            }
            let node = self.node(current).ok_or(SnapshotError::InvalidTree {
                reason: "missing parent",
            })?;
            len = len
                .checked_add(usize::from(node.name_len()))
                .and_then(|n| n.checked_add(1))
                .ok_or(SnapshotError::Overflow {
                    field: "path length",
                })?;
            ancestors.try_reserve(1)?;
            ancestors.push(current);
            current = node.parent();
        }
        let mut path = String::new();
        path.try_reserve_exact(len.max(1))?;
        for ancestor in ancestors.into_iter().rev() {
            path.push('/');
            path.push_str(self.name(ancestor).ok_or(SnapshotError::InvalidTree {
                reason: "invalid name range",
            })?);
        }
        if path.is_empty() {
            path.push('/');
        }
        Ok(Some(path))
    }
}
