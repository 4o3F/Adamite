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

mod bytes;

mod attribute;
mod file_ref;
pub mod fixup;
mod geometry;
mod iter;
mod record;
mod runlist;
mod utf16;

pub use attribute::{
    Attribute, AttributeError, AttributeFlags, AttributeIter, AttributeList, AttributeListEntry,
    AttributeListError, AttributeType, Bitmap, BitmapError, BitmapIter, Data, FileAttributes,
    FileName, FileTimes, IndexAllocation, NonResident, ReparsePoint, StandardInformation,
    StreamSizes, ValueError,
};
pub use file_ref::{
    ATTR_DEF_RECORD, BAD_CLUS_RECORD, BITMAP_RECORD, BOOT_RECORD, EXTEND_RECORD,
    FIRST_NON_RESERVED_RECORD, FileRef, FileRefError, LOG_FILE_RECORD, MFT_MIRROR_RECORD,
    MFT_RECORD, ROOT_RECORD, SECURE_RECORD, UP_CASE_RECORD, VOLUME_RECORD,
};
pub use geometry::{GeometryError, VolumeGeometry, parse_boot_sector};
pub use record::{FileRecord, RecordClass, RecordError, RecordFlags, RecordHeader, classify};
pub use runlist::{Run, Runlist, RunlistError, VcnRange};
pub use utf16::{LossyChars, StrictChars, Units, Utf16Error, Utf16Le};
