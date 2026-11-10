#![doc = include_str!("../README.md")]
#![no_std]
#![forbid(unsafe_code)]
#![deny(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

pub mod attribute;
pub mod boot;
pub mod record;
mod utf16;
mod util;
pub mod value;

pub use attribute::iter::AttributeIter;
pub use attribute::non_resident::{NonResident, StreamSizes};
pub use attribute::runlist::{Run, Runlist, RunlistError, VcnRange};
pub use attribute::{Attribute, AttributeError, AttributeFlags, AttributeType};
pub use boot::{BootError, BootError as GeometryError, VolumeGeometry};
pub use record::fixup;
pub use record::reference::{FileRef, FileRefError};
pub use record::system::{
    ATTR_DEF_RECORD, BAD_CLUS_RECORD, BITMAP_RECORD, BOOT_RECORD, EXTEND_RECORD,
    FIRST_NON_RESERVED_RECORD, LOG_FILE_RECORD, MFT_MIRROR_RECORD, MFT_RECORD, ROOT_RECORD,
    SECURE_RECORD, UP_CASE_RECORD, VOLUME_RECORD,
};
pub use record::{FileRecord, RecordClass, RecordError, RecordFlags, RecordHeader};
pub use utf16::{LossyChars, StrictChars, Units, Utf16Error, Utf16Le};
pub use value::ValueError;
pub use value::attribute_list::{AttributeList, AttributeListEntry, AttributeListError};
pub use value::bitmap::{Bitmap, BitmapError, BitmapIter};
pub use value::data::Data;
pub use value::file_name::FileName;
pub use value::index_allocation::IndexAllocation;
pub use value::reparse_point::ReparsePoint;
pub use value::standard_information::{FileAttributes, FileTimes, StandardInformation};
