//! Owned NTFS contributions retained until extension merge.

use adamite_ntfs::{
    AttributeFlags, AttributeType, FileRef, RecordFlags, StandardInformation, StreamSizes,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecordKind {
    Unused,
    Baad,
    Deleted,
    Base,
    Extension,
    Corrupt,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct RecordSlot {
    pub classification: RecordKind,
    pub sequence: Option<u16>,
    pub flags: Option<RecordFlags>,
    pub base: Option<RecordContribution>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct NameAttribute {
    pub attribute_id: u16,
    pub parent: FileRef,
    pub namespace: u8,
    pub name: Vec<u16>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct StandardAttribute {
    pub attribute_id: u16,
    pub value: StandardInformation,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ReparseAttribute {
    pub attribute_id: u16,
    pub tag: u32,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum StreamStorage {
    Resident {
        size: u64,
    },
    NonResident {
        lowest_vcn: u64,
        highest_vcn: i64,
        compression_unit: u8,
        sizes: Option<StreamSizes>,
        run_count: u64,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct StreamSegment {
    pub source: FileRef,
    pub attribute_type: AttributeType,
    pub name: Vec<u16>,
    pub attribute_id: u16,
    pub flags: AttributeFlags,
    pub storage: StreamStorage,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct RecordContribution {
    pub reference: FileRef,
    pub hard_links: u16,
    pub names: Vec<NameAttribute>,
    pub standard_information: Vec<StandardAttribute>,
    pub streams: Vec<StreamSegment>,
    pub reparse_tags: Vec<ReparseAttribute>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Patch {
    pub base: FileRef,
    pub source: FileRef,
    // Attribute ids are retained on every individual contribution above.
    pub contribution: RecordContribution,
}

/// Flags used by generic tree construction and the eventual snapshot arena.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct EntryFlags(u16);

impl EntryFlags {
    pub const DIRECTORY: Self = Self(1);
    pub const REPARSE: Self = Self(1 << 1);
    pub const SPARSE: Self = Self(1 << 2);
    pub const COMPRESSED: Self = Self(1 << 3);
    pub const HIDDEN: Self = Self(1 << 4);
    pub const SYSTEM: Self = Self(1 << 5);
    pub const HARDLINK_DUP: Self = Self(1 << 6);
    pub const SYNTHETIC: Self = Self(1 << 7);

    pub fn insert(&mut self, flag: Self) {
        self.0 |= flag.0;
    }
    pub fn contains(self, flag: Self) -> bool {
        self.0 & flag.0 == flag.0
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct EntryName {
    pub parent: FileRef,
    pub namespace: u8,
    pub name: Vec<u16>,
}

/// Owned boundary consumed by generic linking and aggregation.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct EntryRecord {
    pub file_ref: FileRef,
    pub attrs: u32,
    pub mtime: u64,
    pub size: u64,
    pub ads_size: u64,
    pub alloc: u64,
    pub names: Vec<EntryName>,
    pub nlink: u16,
    pub reparse_tag: Option<u32>,
    pub flags: EntryFlags,
}
