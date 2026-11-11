//! Canonical `.adm` v1 layout. All integers are little-endian; fields are packed explicitly.
//!
//! The 64-byte header contains magic `ADAMITE\0` (8 bytes), version (u32, 1), header length (u32, 64), file length (u64), section count (u32, 13), flags (u32, zero), CRC-32 (u32 at bytes 32..36), and 28 reserved zero bytes. The IEEE CRC covers the whole file with bytes 32..36 treated as zero.
//!
//! The table starts at byte 64 and ends at byte 480. Its 13 entries are 32 bytes each: kind and zero flags (u32 each), offset, length and count (u64 each). Kinds 1 through 13 are Volume, Stats, Nodes, Names, Children, AdsSize, FileRefs, Attrs, Nlink, ReparseTags, TopFiles, ExtStats and ExtNames. The first section starts at 480; each subsequent offset is the previous end rounded up to 8 bytes. Padding is minimal and zero; the file ends at the last section's end.
//!
//! Volume has count 1 and a fixed part of 69 bytes: geometry is bytes_per_sector (u16), sectors_per_cluster, file_record_size, index_record_size (u32 each), total_sectors, mft_lcn, mftmirr_lcn, serial (u64 each), totaling 46 bytes. Next are scanned_at_filetime and free_bytes, each a one-byte presence tag and an always-present u64. Label is a one-byte tag, always-present u32 UTF-8 byte length, then label bytes. Absent payloads are zero; a present empty label has tag 1 and length 0.
//!
//! Stats has count 26 and exactly 208 bytes: u64 counters in this fixed order, independent of Rust declaration order: records_total, records_in_use, base_records, extension_records, unused, deleted, baad, corrupt_records, bad_fixup, invalid_fixup_metadata, invalid_signature, corrupt_headers, corrupt_attributes, type_storage_mismatches, corrupt_values, corrupt_runlists, out_of_volume_extents, invalid_attribute_lists, invalid_file_refs, invalid_utf16, rejected_extensions, corrupt_streams, orphans, cycle_nodes, hardlink_duplicates, metafiles.
//!
//! Nodes are 64 bytes each: size, alloc, total_size, total_alloc, mtime (u64), parent, child_start, child_len, file_count, name_off (u32), name_len and flags (u16). Children and TopFiles hold u32 node identifiers; AdsSize holds u64, Attrs u32, and Nlink u16. Their counts are element counts. Names is the tightly packed name pool in node order; its count equals its byte length.
//!
//! FileRefs (packed u64) and ReparseTags (u32) have one optional value per node. Their LSB-first bitmap occupies ceil(count / 8) bytes with unused bits zero; the full value array starts at the bitmap length rounded up to 8 bytes. Internal padding and absent values are zero; section length is exactly the value-array end.
//!
//! ExtStats entries are 32 bytes: count, total_size, total_alloc (u64), name_off and name_len (u32). ExtNames concatenates UTF-8 extension strings in ExtStats order without gaps; its count equals its byte length. Empty extensions occupy no bytes.

use super::PersistError;

pub(super) const MAGIC: &[u8; 8] = &crate::scan::ScanOutput::ADM_MAGIC;
pub(super) const VERSION: u32 = 1;
pub(super) const HEADER_LEN: usize = 64;
pub(super) const TABLE_ENTRY_LEN: usize = 32;
pub(super) const SECTION_COUNT: usize = 13;
pub(super) const TABLE_END: usize = HEADER_LEN + TABLE_ENTRY_LEN * SECTION_COUNT;
pub(super) const STATS_COUNT: usize = 26;
pub(super) const VOLUME_FIXED_LEN: usize = 69;

#[derive(Debug, Clone, Copy)]
#[repr(u32)]
pub(super) enum SectionKind {
    Volume = 1,
    Stats,
    Nodes,
    Names,
    Children,
    AdsSize,
    FileRefs,
    Attrs,
    Nlink,
    ReparseTags,
    TopFiles,
    ExtStats,
    ExtNames,
}

pub(super) struct Writer<'a> {
    bytes: &'a mut [u8],
    cursor: usize,
}

impl<'a> Writer<'a> {
    pub(super) fn align_up(value: usize, alignment: usize) -> Result<usize, PersistError> {
        let mask = alignment.checked_sub(1).ok_or(PersistError::Overflow {
            operation: "alignment",
        })?;
        value
            .checked_add(mask)
            .map(|value| value & !mask)
            .ok_or(PersistError::Overflow {
                operation: "alignment",
            })
    }

    pub(super) fn new(bytes: &'a mut [u8]) -> Self {
        Self { bytes, cursor: 0 }
    }

    pub(super) fn bytes(&mut self, value: &[u8]) -> Result<(), PersistError> {
        let end = self
            .cursor
            .checked_add(value.len())
            .ok_or(PersistError::Overflow {
                operation: "write end",
            })?;
        self.bytes
            .get_mut(self.cursor..end)
            .ok_or(PersistError::Overflow {
                operation: "write range",
            })?
            .copy_from_slice(value);
        self.cursor = end;
        Ok(())
    }

    pub(super) fn u8(&mut self, value: u8) -> Result<(), PersistError> {
        self.bytes(&[value])
    }

    pub(super) fn u16(&mut self, value: u16) -> Result<(), PersistError> {
        self.bytes(&value.to_le_bytes())
    }

    pub(super) fn u32(&mut self, value: u32) -> Result<(), PersistError> {
        self.bytes(&value.to_le_bytes())
    }

    pub(super) fn u64(&mut self, value: u64) -> Result<(), PersistError> {
        self.bytes(&value.to_le_bytes())
    }

    pub(super) fn pad_to(&mut self, offset: usize) -> Result<(), PersistError> {
        self.bytes
            .get_mut(self.cursor..offset)
            .ok_or(PersistError::Overflow {
                operation: "padding range",
            })?
            .fill(0);
        self.cursor = offset;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub(super) struct Crc32 {
    hasher: crc32fast::Hasher,
    position: usize,
}
impl Crc32 {
    pub(super) fn new() -> Self {
        Self {
            hasher: crc32fast::Hasher::new(),
            position: 0,
        }
    }
    pub(super) fn update(&mut self, bytes: &[u8]) -> Result<(), PersistError> {
        let end = self
            .position
            .checked_add(bytes.len())
            .ok_or(PersistError::Overflow {
                operation: "checksum position",
            })?;
        let prefix = 32usize.saturating_sub(self.position).min(bytes.len());
        self.hasher
            .update(bytes.get(..prefix).ok_or(PersistError::Overflow {
                operation: "checksum prefix",
            })?);
        let zero_end = 36usize.saturating_sub(self.position).min(bytes.len());
        if zero_end > prefix {
            let len = zero_end.checked_sub(prefix).ok_or(PersistError::Overflow {
                operation: "checksum slot",
            })?;
            self.hasher
                .update([0u8; 4].get(..len).ok_or(PersistError::Overflow {
                    operation: "checksum slot",
                })?);
        }
        self.hasher.update(
            bytes
                .get(zero_end.max(prefix)..)
                .ok_or(PersistError::Overflow {
                    operation: "checksum suffix",
                })?,
        );
        self.position = end;
        Ok(())
    }
    pub(super) fn finish(self) -> u32 {
        self.hasher.finalize()
    }
}

#[cfg(test)]
mod tests {
    use super::Crc32;
    use std::error::Error;

    #[test]
    fn incremental_crc_matches_one_shot_with_split_checksum_slot() -> Result<(), Box<dyn Error>> {
        let bytes: Vec<u8> = (0u8..=255).cycle().take(12_345).collect();
        let mut zeroed = bytes.clone();
        zeroed.get_mut(32..36).ok_or("missing checksum")?.fill(0);
        for size in [1, 7, 4096, bytes.len()] {
            let mut crc = Crc32::new();
            for block in bytes.chunks(size) {
                crc.update(block)?;
            }
            assert_eq!(crc.finish(), crc32fast::hash(&zeroed), "block size {size}");
        }
        Ok(())
    }
}
