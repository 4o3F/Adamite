//! Fixed-size hot nodes, stable identifiers and read-only node views.

use super::SnapshotError;
use adamite_ntfs::record::reference::FileRef;

/// Index into one snapshot's node arena; identifiers are local to that snapshot.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(u32);

impl NodeId {
    pub(crate) fn try_from_index(value: usize, field: &'static str) -> Result<Self, SnapshotError> {
        u32::try_from(value)
            .map(Self)
            .map_err(|_| SnapshotError::overflow(field))
    }
    /// The filesystem root in every nonempty snapshot.
    pub const ROOT: Self = Self(0);
    /// Wraps an arena index; lookup checks whether it exists in a particular snapshot.
    #[must_use]
    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }
    /// Returns the arena index.
    #[must_use]
    pub const fn raw(self) -> u32 {
        self.0
    }
}

/// Independent node properties carried in the compact arena.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct NodeFlags(u16);

impl NodeFlags {
    /// Directory entry.
    pub const DIRECTORY: Self = Self(1 << 0);
    /// Reparse point; its target is not followed.
    pub const REPARSE: Self = Self(1 << 1);
    /// At least one sparse stream.
    pub const SPARSE: Self = Self(1 << 2);
    /// At least one compressed stream.
    pub const COMPRESSED: Self = Self(1 << 3);
    /// Hidden file attribute.
    pub const HIDDEN: Self = Self(1 << 4);
    /// System file attribute.
    pub const SYSTEM: Self = Self(1 << 5);
    /// Secondary hard link whose contribution is suppressed during aggregation.
    pub const HARDLINK_DUP: Self = Self(1 << 6);
    /// Generated directory without an on-disk file reference.
    pub const SYNTHETIC: Self = Self(1 << 7);
    /// Returns the original flag bits.
    #[must_use]
    pub const fn raw(self) -> u16 {
        self.0
    }
    /// Tests whether all supplied properties are present.
    #[must_use]
    pub const fn contains(self, flags: Self) -> bool {
        self.0 & flags.0 == flags.0
    }
    pub(crate) fn insert(&mut self, flags: Self) {
        self.0 |= flags.0;
    }
}

/// A 64-byte hot node; names and less frequently read metadata live in shared arrays.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Node {
    pub(super) size: u64,
    pub(super) alloc: u64,
    pub(super) total_size: u64,
    pub(super) total_alloc: u64,
    pub(super) mtime: u64,
    pub(super) parent: u32,
    pub(super) child_start: u32,
    pub(super) child_len: u32,
    pub(super) file_count: u32,
    pub(super) name_off: u32,
    pub(super) name_len: u16,
    pub(super) flags: u16,
}

const _: () = assert!(std::mem::size_of::<Node>() == 64);

impl Node {
    pub(super) fn has(&self, flag: NodeFlags) -> bool {
        self.flags & flag.raw() != 0
    }

    pub(super) fn is_primary_file(&self) -> bool {
        !self.has(NodeFlags::DIRECTORY)
            && !self.has(NodeFlags::SYNTHETIC)
            && !self.has(NodeFlags::HARDLINK_DUP)
    }
}

/// Borrowed hot fields and the corresponding cold metadata for one arena node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeView<'a> {
    pub(super) id: NodeId,
    pub(super) node: &'a Node,
    pub(super) ads_size: u64,
    pub(super) file_ref: Option<FileRef>,
    pub(super) attrs: u32,
    pub(super) nlink: u16,
    pub(super) reparse_tag: Option<u32>,
}

impl NodeView<'_> {
    /// Returns the identifier within this snapshot.
    #[must_use]
    pub const fn id(self) -> NodeId {
        self.id
    }
    /// Returns unnamed DATA logical bytes.
    #[must_use]
    pub const fn size(self) -> u64 {
        self.node.size
    }
    /// Returns allocation across all non-resident streams.
    #[must_use]
    pub const fn alloc(self) -> u64 {
        self.node.alloc
    }
    /// Returns subtree unnamed DATA bytes; duplicate links are excluded from ancestor totals.
    #[must_use]
    pub const fn total_size(self) -> u64 {
        self.node.total_size
    }
    /// Returns subtree allocation bytes, including directory self-allocation.
    #[must_use]
    pub const fn total_alloc(self) -> u64 {
        self.node.total_alloc
    }
    /// Returns the modification time as a raw FILETIME.
    #[must_use]
    pub const fn mtime(self) -> u64 {
        self.node.mtime
    }
    /// Returns the parent identifier; root is its own parent.
    #[must_use]
    pub const fn parent(self) -> NodeId {
        NodeId::from_raw(self.node.parent)
    }
    /// Returns the start of the child range in CSR storage.
    #[must_use]
    pub const fn child_start(self) -> u32 {
        self.node.child_start
    }
    /// Returns the number of immediate children.
    #[must_use]
    pub const fn child_len(self) -> u32 {
        self.node.child_len
    }
    /// Returns the subtree count of primary files.
    #[must_use]
    pub const fn file_count(self) -> u32 {
        self.node.file_count
    }
    /// Returns the name byte offset in the shared UTF-8 pool.
    #[must_use]
    pub const fn name_off(self) -> u32 {
        self.node.name_off
    }
    /// Returns the name length in UTF-8 bytes.
    #[must_use]
    pub const fn name_len(self) -> u16 {
        self.node.name_len
    }
    /// Returns the node properties.
    #[must_use]
    pub const fn flags(self) -> NodeFlags {
        NodeFlags(self.node.flags)
    }
    /// Returns named DATA logical bytes, excluding $BadClus:$Bad.
    #[must_use]
    pub const fn ads_size(self) -> u64 {
        self.ads_size
    }
    /// Returns the on-disk reference, absent for synthetic directories.
    #[must_use]
    pub const fn file_ref(self) -> Option<FileRef> {
        self.file_ref
    }
    /// Returns the raw STANDARD_INFORMATION attributes.
    #[must_use]
    pub const fn attrs(self) -> u32 {
        self.attrs
    }
    /// Returns the on-disk hard-link count.
    #[must_use]
    pub const fn nlink(self) -> u16 {
        self.nlink
    }
    /// Returns the reparse tag when present.
    #[must_use]
    pub const fn reparse_tag(self) -> Option<u32> {
        self.reparse_tag
    }
}
