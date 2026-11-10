//! Owned NTFS contributions retained until extension merge.

use adamite_ntfs::{
    attribute::{AttributeFlags, AttributeType, non_resident::StreamSizes},
    record::{RecordFlags, reference::FileRef},
    value::standard_information::StandardInformation,
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
    pub parent: FileRef,
    pub namespace: u8,
    pub name: Vec<u16>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct StandardAttribute {
    pub value: StandardInformation,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ReparseAttribute {
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
    // Stream attribute ids are retained for segment identity.
    pub contribution: RecordContribution,
}
