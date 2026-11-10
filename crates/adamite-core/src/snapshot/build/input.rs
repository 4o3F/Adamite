//! Resolved graph input and checked name ranges for snapshot construction.

use crate::snapshot::{NodeFlags, SnapshotError};
use adamite_ntfs::record::reference::FileRef;

/// One node in a flat parent-index array.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ResolvedNode {
    pub(crate) parent: usize,
    pub(crate) file_ref: Option<FileRef>,
    pub(crate) original_parent: Option<FileRef>,
    pub(crate) name_off: u32,
    pub(crate) name_len: u16,
    pub(crate) flags: NodeFlags,
    pub(crate) attrs: u32,
    pub(crate) mtime: u64,
    pub(crate) size: u64,
    pub(crate) ads_size: u64,
    pub(crate) alloc: u64,
    pub(crate) nlink: u16,
    pub(crate) reparse_tag: Option<u32>,
}

/// A flat Vec of nodes whose parent fields are indices into the same array.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct ParentTree {
    pub(crate) nodes: Vec<ResolvedNode>,
    pub(crate) names: Vec<u8>,
}

/// Checked byte offset and length within the shared UTF-8 name pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NameRange {
    offset: u32,
    len: u16,
}

impl NameRange {
    pub(crate) fn new(offset: usize, len: usize) -> Result<Self, SnapshotError> {
        let end = offset
            .checked_add(len)
            .ok_or_else(|| SnapshotError::overflow("name pool length"))?;
        u32::try_from(end).map_err(|_| SnapshotError::overflow("name pool length"))?;
        Ok(Self {
            offset: u32::try_from(offset).map_err(|_| SnapshotError::overflow("name offset"))?,
            len: u16::try_from(len).map_err(|_| SnapshotError::overflow("name length"))?,
        })
    }

    pub(crate) fn into_parts(self) -> (u32, u16) {
        (self.offset, self.len)
    }
}
