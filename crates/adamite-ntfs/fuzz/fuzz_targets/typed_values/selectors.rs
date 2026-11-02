//! Wire selectors for typed-value fuzz inputs, shared with extraction and seed validation.
// Keep decimal u8 literals: scripts/fuzz_corpus.ts reads these documented names.

/// STANDARD_INFORMATION value bytes follow the selector.
pub const STANDARD_INFORMATION: u8 = 0;
/// FILE_NAME value bytes, including their embedded UTF-16 name, follow the selector.
pub const FILE_NAME: u8 = 1;
/// REPARSE_POINT value bytes follow the selector.
pub const REPARSE_POINT: u8 = 2;
/// A complete ATTRIBUTE_LIST value follows the selector.
pub const ATTRIBUTE_LIST: u8 = 3;
/// A little-endian u64 logical bit count precedes the bitmap bytes.
pub const BITMAP: u8 = 4;
/// Raw UTF-16LE bytes follow the selector.
pub const UTF16: u8 = 5;
/// A DATA attribute region, including its end marker, follows the selector.
pub const DATA: u8 = 6;
/// An INDEX_ALLOCATION attribute region, including its end marker, follows the selector.
pub const INDEX_ALLOCATION: u8 = 7;
/// Selectors are normalized modulo this number of supported decoders.
pub const COUNT: u8 = 8;
