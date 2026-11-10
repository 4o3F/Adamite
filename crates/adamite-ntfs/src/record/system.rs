//! Well-known NTFS system record numbers.

/// The `$MFT` record number.
pub const MFT: u64 = 0;
/// The `$MFTMirr` record number.
pub const MFT_MIRROR: u64 = 1;
/// The `$LogFile` record number.
pub const LOG_FILE: u64 = 2;
/// The `$Volume` record number.
pub const VOLUME: u64 = 3;
/// The `$AttrDef` record number.
pub const ATTR_DEF: u64 = 4;
/// The root directory record number.
pub const ROOT: u64 = 5;
/// The `$Bitmap` record number.
pub const BITMAP: u64 = 6;
/// The `$Boot` record number.
pub const BOOT: u64 = 7;
/// The `$BadClus` record number.
pub const BAD_CLUS: u64 = 8;
/// The `$Secure` record number.
pub const SECURE: u64 = 9;
/// The `$UpCase` record number.
pub const UP_CASE: u64 = 10;
/// The `$Extend` record number.
pub const EXTEND: u64 = 11;
/// The first record number outside the reserved system-record range.
pub const FIRST_NON_RESERVED: u64 = 16;
