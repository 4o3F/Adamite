//! Rejection and canonicality checks for untrusted v1 snapshot bytes.

#[path = "support/persist_blocks.rs"]
mod persist_blocks;
mod support;

use adamite_core::{
    scan::ScanContext,
    snapshot::{NodeFlags, PersistError},
};
use adamite_fixtures::Fixture;
use adamite_ntfs::boot::{BootError, GeometryField};
use proptest::{prelude::*, test_runner::Config};
use std::path::Path;
use support::{TestResult, scan, section, tiny_image, u32_at, u64_at};

const DIRECTORY: u16 = NodeFlags::DIRECTORY.raw();
const HARDLINK_DUP: u16 = NodeFlags::HARDLINK_DUP.raw();
const SYNTHETIC: u16 = NodeFlags::SYNTHETIC.raw();

fn put(bytes: &mut [u8], offset: usize, value: &[u8]) -> TestResult {
    bytes
        .get_mut(offset..offset + value.len())
        .ok_or("write outside encoding")?
        .copy_from_slice(value);
    Ok(())
}

fn repair_crc(bytes: &mut [u8]) -> TestResult {
    put(bytes, 32, &[0; 4])?;
    let crc = crc32fast::hash(bytes);
    put(bytes, 32, &crc.to_le_bytes())
}

fn repair_file_len(bytes: &mut [u8]) -> TestResult {
    put(bytes, 16, &u64::try_from(bytes.len())?.to_le_bytes())?;
    repair_crc(bytes)
}

struct Encoding {
    bytes: Vec<u8>,
}

impl Encoding {
    fn tiny() -> TestResult<Self> {
        let output = scan(
            &tiny_image(),
            ScanContext {
                label: Some("tiny".to_owned()),
                scanned_at_filetime: Some(123),
                free_bytes: Some(456),
            },
        )?;
        Ok(Self {
            bytes: output.to_bytes()?,
        })
    }

    fn basic() -> TestResult<Self> {
        let fixture = Fixture::load(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/ntfs/basic-4k.json"),
        )?;
        let output = scan(fixture.image(), ScanContext::default())?;
        Ok(Self {
            bytes: output.to_bytes()?,
        })
    }

    fn at(&self, kind: usize, relative: usize) -> TestResult<usize> {
        Ok(section(&self.bytes, kind)?.0 + relative)
    }

    fn write(&mut self, kind: usize, relative: usize, value: &[u8]) -> TestResult {
        let at = self.at(kind, relative)?;
        put(&mut self.bytes, at, value)
    }

    fn node32(&mut self, node: usize, field: usize, value: u32) -> TestResult {
        self.write(3, node * 64 + field, &value.to_le_bytes())
    }

    fn node64(&mut self, node: usize, field: usize, value: u64) -> TestResult {
        self.write(3, node * 64 + field, &value.to_le_bytes())
    }

    fn node_flags(&self, node: usize) -> TestResult<u16> {
        let at = self.at(3, node * 64 + 62)?;
        Ok(u16::from_le_bytes(
            self.bytes
                .get(at..at + 2)
                .ok_or("missing flags")?
                .try_into()?,
        ))
    }

    fn flags(&mut self, node: usize, value: u16) -> TestResult {
        self.write(3, node * 64 + 62, &value.to_le_bytes())
    }

    fn toggle_flags(&mut self, node: usize, bits: u16) -> TestResult {
        self.flags(node, self.node_flags(node)? ^ bits)
    }

    fn optional_at(&self, kind: usize, node: usize, width: usize) -> TestResult<usize> {
        let (start, _, count) = section(&self.bytes, kind)?;
        Ok(start + ((count.div_ceil(8) + 7) & !7) + node * width)
    }

    fn reference(&mut self, node: usize, value: u64) -> TestResult {
        let at = self.optional_at(7, node, 8)?;
        put(&mut self.bytes, at, &value.to_le_bytes())
    }

    fn replace_section(&mut self, kind: usize, data: &[u8], count: usize) -> TestResult {
        let mut bytes = self.bytes.get(..480).ok_or("missing table")?.to_vec();
        for current in 1..=13 {
            bytes.resize((bytes.len() + 7) & !7, 0);
            let offset = bytes.len();
            let (old_offset, old_len, old_count) = section(&self.bytes, current)?;
            let (payload, elements) = if current == kind {
                (data, count)
            } else {
                (
                    self.bytes
                        .get(old_offset..old_offset + old_len)
                        .ok_or("missing section")?,
                    old_count,
                )
            };
            bytes.extend_from_slice(payload);
            let entry = 64 + 32 * (current - 1);
            for (field, value) in [(8, offset), (16, payload.len()), (24, elements)] {
                put(
                    &mut bytes,
                    entry + field,
                    &u64::try_from(value)?.to_le_bytes(),
                )?;
            }
        }
        repair_file_len(&mut bytes)?;
        self.bytes = bytes;
        Ok(())
    }

    fn rename(&mut self, node: usize, name: &str) -> TestResult {
        let (start, _, count) = section(&self.bytes, 3)?;
        let (pool, pool_len, _) = section(&self.bytes, 4)?;
        let names = self
            .bytes
            .get(pool..pool + pool_len)
            .ok_or("missing names")?
            .to_vec();
        let mut packed = Vec::new();
        for index in 0..count {
            let offset = usize::try_from(u32_at(&self.bytes, start + index * 64 + 56)?)?;
            let at = start + index * 64 + 60;
            let len = usize::from(u16::from_le_bytes(
                self.bytes
                    .get(at..at + 2)
                    .ok_or("missing name length")?
                    .try_into()?,
            ));
            let value = if index == node {
                name.as_bytes()
            } else {
                names.get(offset..offset + len).ok_or("missing name")?
            };
            self.node32(index, 56, u32::try_from(packed.len())?)?;
            self.write(
                3,
                index * 64 + 60,
                &u16::try_from(value.len())?.to_le_bytes(),
            )?;
            packed.extend_from_slice(value);
        }
        self.replace_section(4, &packed, packed.len())
    }

    fn children(&self) -> TestResult<Vec<Vec<u32>>> {
        let (nodes, _, count) = section(&self.bytes, 3)?;
        let (children, _, _) = section(&self.bytes, 5)?;
        (0..count)
            .map(|index| {
                let start = usize::try_from(u32_at(&self.bytes, nodes + index * 64 + 44)?)?;
                let len = usize::try_from(u32_at(&self.bytes, nodes + index * 64 + 48)?)?;
                (start..start + len)
                    .map(|edge| u32_at(&self.bytes, children + edge * 4))
                    .collect()
            })
            .collect()
    }

    fn set_children(&mut self, lists: &[Vec<u32>]) -> TestResult {
        let mut edges = Vec::new();
        for (index, children) in lists.iter().enumerate() {
            self.node32(index, 44, u32::try_from(edges.len() / 4)?)?;
            self.node32(index, 48, u32::try_from(children.len())?)?;
            for &child in children {
                self.node32(usize::try_from(child)?, 40, u32::try_from(index)?)?;
                edges.extend_from_slice(&child.to_le_bytes());
            }
        }
        self.replace_section(5, &edges, edges.len() / 4)
    }

    fn move_child(&mut self, child: u32, parent: usize) -> TestResult {
        let mut lists = self.children()?;
        for children in &mut lists {
            children.retain(|&id| id != child);
        }
        lists.get_mut(parent).ok_or("missing parent")?.push(child);
        self.set_children(&lists)
    }

    fn tree_error(mut self, node: u32, reason: &'static str) -> TestResult {
        repair_crc(&mut self.bytes)?;
        let result = persist_blocks::decode_checked(&self.bytes);
        assert!(
            matches!(result, Err(PersistError::InvalidTree { node: actual_node, reason: actual_reason }) if actual_node == node && actual_reason == reason),
            "expected node {node}: {reason}, got {result:?}"
        );
        Ok(())
    }

    fn index_error(mut self, reason: &'static str) -> TestResult {
        repair_crc(&mut self.bytes)?;
        let result = persist_blocks::decode_checked(&self.bytes);
        assert!(
            matches!(result, Err(PersistError::InvalidIndex { reason: actual }) if actual == reason),
            "expected {reason}, got {result:?}"
        );
        Ok(())
    }

    fn section_error(mut self, section: u32, reason: &'static str) -> TestResult {
        repair_crc(&mut self.bytes)?;
        let result = persist_blocks::decode_checked(&self.bytes);
        assert!(
            matches!(result, Err(PersistError::InvalidSection { section: actual_section, reason: actual_reason }) if actual_section == section && actual_reason == reason),
            "expected section {section}: {reason}, got {result:?}"
        );
        Ok(())
    }

    fn table_error(mut self, reason: &'static str) -> TestResult {
        repair_crc(&mut self.bytes)?;
        let result = persist_blocks::decode_checked(&self.bytes);
        assert!(
            matches!(result, Err(PersistError::InvalidSectionTable { reason: actual }) if actual == reason),
            "expected {reason}, got {result:?}"
        );
        Ok(())
    }
}

#[test]
fn header_magic_and_version() -> TestResult {
    let mut file = Encoding::tiny()?;
    file.bytes[0] ^= 1;
    repair_crc(&mut file.bytes)?;
    assert!(matches!(
        persist_blocks::decode_checked(&file.bytes),
        Err(PersistError::BadMagic)
    ));
    let mut file = Encoding::tiny()?;
    put(&mut file.bytes, 8, &2u32.to_le_bytes())?;
    repair_crc(&mut file.bytes)?;
    assert!(matches!(
        persist_blocks::decode_checked(&file.bytes),
        Err(PersistError::UnsupportedVersion { version: 2 })
    ));
    Ok(())
}

#[test]
fn header_lengths_flags_and_reserved_bytes() -> TestResult {
    for (offset, value, reason) in [
        (12, 63, "header length is not 64"),
        (24, 12, "section count is not 13"),
        (28, 1, "nonzero header flags"),
    ] {
        let mut file = Encoding::tiny()?;
        put(&mut file.bytes, offset, &u32::to_le_bytes(value))?;
        repair_crc(&mut file.bytes)?;
        assert!(
            matches!(persist_blocks::decode_checked(&file.bytes), Err(PersistError::InvalidHeader { reason: actual }) if actual == reason)
        );
    }
    for offset in 36..64 {
        let mut file = Encoding::tiny()?;
        file.bytes[offset] = 1;
        repair_crc(&mut file.bytes)?;
        assert!(matches!(
            persist_blocks::decode_checked(&file.bytes),
            Err(PersistError::InvalidHeader {
                reason: "nonzero reserved header bytes"
            })
        ));
    }
    Ok(())
}

#[test]
fn header_file_len_and_short_input() -> TestResult {
    let original = Encoding::tiny()?.bytes;
    let mut bytes = original.clone();
    put(
        &mut bytes,
        16,
        &u64::try_from(original.len() - 1)?.to_le_bytes(),
    )?;
    repair_crc(&mut bytes)?;
    assert!(matches!(
        persist_blocks::decode_checked(&bytes),
        Err(PersistError::InvalidHeader {
            reason: "trailing bytes after file length"
        })
    ));
    put(
        &mut bytes,
        16,
        &u64::try_from(original.len() + 1)?.to_le_bytes(),
    )?;
    repair_crc(&mut bytes)?;
    assert!(
        matches!(persist_blocks::decode_checked(&bytes), Err(PersistError::Truncated { needed, available }) if needed == original.len() + 1 && available == original.len())
    );
    // Magic is checked before the minimum header length, including inputs shorter than eight bytes.
    for len in 0..64 {
        let result = persist_blocks::decode_checked(&original[..len]);
        if len < 8 {
            assert!(matches!(result, Err(PersistError::BadMagic)));
        } else {
            assert!(
                matches!(result, Err(PersistError::Truncated { needed: 64, available }) if available == len)
            );
        }
    }
    Ok(())
}

#[test]
fn table_kind_order_unknown_kind_and_entry_flags() -> TestResult {
    for kind in 1..=13 {
        for value in [0u32, 14, u32::try_from(kind % 13 + 1)?] {
            let mut file = Encoding::tiny()?;
            put(&mut file.bytes, 64 + (kind - 1) * 32, &value.to_le_bytes())?;
            file.table_error("section kinds are not 1 through 13 in order")?;
        }
        let mut file = Encoding::tiny()?;
        put(
            &mut file.bytes,
            64 + (kind - 1) * 32 + 4,
            &1u32.to_le_bytes(),
        )?;
        file.table_error("nonzero section flags")?;
    }
    Ok(())
}

#[test]
fn table_first_offset_and_extra_padding_block() -> TestResult {
    let mut file = Encoding::tiny()?;
    put(&mut file.bytes, 72, &488u64.to_le_bytes())?;
    file.table_error("noncanonical section placement")?;
    let mut file = Encoding::tiny()?;
    let shifted_kind = 6;
    let at = file.at(shifted_kind, 0)?;
    file.bytes.splice(at..at, [0; 8]);
    for kind in shifted_kind..=13 {
        let entry = 64 + (kind - 1) * 32 + 8;
        let shifted = u64_at(&file.bytes, entry)? + 8;
        put(&mut file.bytes, entry, &shifted.to_le_bytes())?;
    }
    repair_file_len(&mut file.bytes)?;
    file.table_error("noncanonical section placement")
}

#[test]
fn table_nonzero_padding_and_trailing_bytes() -> TestResult {
    let mut file = Encoding::tiny()?;
    let (start, len, _) = section(&file.bytes, 1)?;
    assert!(start + len < file.at(2, 0)?);
    file.bytes[start + len] = 1;
    file.table_error("nonzero section padding")?;
    let mut file = Encoding::tiny()?;
    file.bytes.push(0);
    repair_file_len(&mut file.bytes)?;
    file.table_error("last section does not end at file length")
}

#[test]
fn section_length_formulas() -> TestResult {
    for kind in [3, 4, 5, 6, 8, 9, 11, 12, 13, 7, 10] {
        let mut file = Encoding::tiny()?;
        let entry = 64 + (kind - 1) * 32 + 24;
        let count = u64_at(&file.bytes, entry)? + 1;
        put(&mut file.bytes, entry, &count.to_le_bytes())?;
        file.section_error(u32::try_from(kind)?, "section length does not match count")?;
    }
    let mut file = Encoding::tiny()?;
    file.write(1, 65, &3u32.to_le_bytes())?;
    file.section_error(1, "volume length does not match label")?;
    let mut file = Encoding::tiny()?;
    put(&mut file.bytes, 64 + 32 + 24, &25u64.to_le_bytes())?;
    file.section_error(2, "invalid stats count or length")?;
    let mut file = Encoding::tiny()?;
    put(&mut file.bytes, 64 + 24, &2u64.to_le_bytes())?;
    file.section_error(1, "invalid volume count or length")
}

#[test]
fn truncation_at_every_section_start_and_end() -> TestResult {
    let original = Encoding::tiny()?.bytes;
    for kind in 1..=13 {
        let (offset, len, _) = section(&original, kind)?;
        for boundary in [offset, offset + len] {
            // The final end is the complete valid file; remove its last byte to exercise truncation.
            let cut = boundary.min(original.len() - 1);
            for repair in [false, true] {
                let mut bytes = original[..cut].to_vec();
                if repair {
                    repair_file_len(&mut bytes)?;
                }
                assert!(
                    matches!(
                        persist_blocks::decode_checked(&bytes),
                        Err(PersistError::Truncated { .. })
                    ),
                    "section {kind}, boundary {boundary}, repair {repair}"
                );
            }
        }
    }
    Ok(())
}

#[test]
fn checksum_reports_stored_and_computed_values() -> TestResult {
    let mut file = Encoding::tiny()?;
    let expected = u32_at(&file.bytes, 32)?;
    file.write(2, 0, &999u64.to_le_bytes())?;
    let mut zeroed = file.bytes.clone();
    put(&mut zeroed, 32, &[0; 4])?;
    let actual = crc32fast::hash(&zeroed);
    assert_ne!(expected, actual);
    assert!(
        matches!(persist_blocks::decode_checked(&file.bytes), Err(PersistError::ChecksumMismatch { expected: stored, actual: computed }) if stored == expected && computed == actual)
    );
    Ok(())
}

#[test]
fn volume_presence_tags_and_absent_payloads() -> TestResult {
    for tag in [46, 55, 64] {
        let mut file = Encoding::tiny()?;
        file.write(1, tag, &[2])?;
        file.section_error(1, "presence tag is not zero or one")?;
    }
    for tag in [46, 55] {
        let mut file = Encoding::tiny()?;
        file.write(1, tag, &[0])?;
        file.section_error(1, "nonzero absent payload")?;
    }
    let mut file = Encoding::tiny()?;
    file.write(1, 64, &[0])?;
    file.section_error(1, "nonzero absent label length")
}

#[test]
fn optional_bitmaps_padding_and_absent_values() -> TestResult {
    for (kind, width) in [(7, 8), (10, 4)] {
        let mut file = Encoding::tiny()?;
        let at = file.at(kind, 0)?;
        file.bytes[at] |= 0x80;
        file.section_error(u32::try_from(kind)?, "nonzero unused bitmap bits")?;
        let mut file = Encoding::tiny()?;
        file.write(kind, 1, &[1])?;
        file.section_error(u32::try_from(kind)?, "nonzero padding")?;
        let mut file = Encoding::tiny()?;
        let at = file.optional_at(kind, 1, width)?;
        file.bytes[at] = 1;
        file.section_error(u32::try_from(kind)?, "nonzero absent column value")?;
    }
    Ok(())
}

#[test]
fn extension_name_offsets_gap_and_overlap() -> TestResult {
    let mut gap = Encoding::tiny()?;
    gap.write(12, 24, &1u32.to_le_bytes())?;
    gap.section_error(12, "extension names are not contiguous")?;
    let mut overlap = Encoding::basic()?;
    let (start, _, count) = section(&overlap.bytes, 12)?;
    let index = (0..count)
        .find(|&index| {
            u32_at(&overlap.bytes, start + index * 32 + 24).is_ok_and(|offset| offset > 0)
        })
        .ok_or("no nonempty preceding extension")?;
    let offset = u32_at(&overlap.bytes, start + index * 32 + 24)?;
    overlap.write(12, index * 32 + 24, &(offset - 1).to_le_bytes())?;
    overlap.section_error(12, "extension names are not contiguous")
}

#[test]
fn label_utf8_and_geometry_field() -> TestResult {
    let mut file = Encoding::tiny()?;
    file.write(1, 69, &[0xff])?;
    repair_crc(&mut file.bytes)?;
    assert!(matches!(
        persist_blocks::decode_checked(&file.bytes),
        Err(PersistError::Utf8 { section: 1, .. })
    ));
    let mut file = Encoding::tiny()?;
    file.write(1, 0, &513u16.to_le_bytes())?;
    repair_crc(&mut file.bytes)?;
    assert!(matches!(
        persist_blocks::decode_checked(&file.bytes),
        Err(PersistError::Geometry {
            source: BootError::InvalidField {
                field: GeometryField::BytesPerSector,
                value: 513
            }
        })
    ));
    Ok(())
}

macro_rules! tiny_tree_test {
    ($name:ident, $node:expr, $reason:literal, |$file:ident| $mutation:block) => {
        #[test]
        fn $name() -> TestResult {
            let mut $file = Encoding::tiny()?;
            $mutation
            $file.tree_error($node, $reason)
        }
    };
}

#[test]
fn zero_node_count() -> TestResult {
    let mut file = Encoding::tiny()?;
    file.replace_section(3, &[], 0)?;
    file.tree_error(0, "node count outside 1..=u32::MAX")
}

#[test]
fn cold_column_lengths() -> TestResult {
    for (kind, width) in [(6, 8), (7, 8), (8, 4), (9, 2), (10, 4)] {
        let mut file = Encoding::tiny()?;
        let start = file.at(kind, 0)?;
        let len = 3 * width + if matches!(kind, 7 | 10) { 8 } else { 0 };
        let data = file
            .bytes
            .get(start..start + len)
            .ok_or("missing column")?
            .to_vec();
        if kind == 7 {
            let mut data = data;
            data[0] &= 0b111;
            file.replace_section(kind, &data, 3)?;
        } else {
            file.replace_section(kind, &data, 3)?;
        }
        file.tree_error(0, "cold column length differs from node count")?;
    }
    Ok(())
}

#[test]
fn oversized_declared_node_count_and_name_pool() -> TestResult {
    let mut file = Encoding::tiny()?;
    put(
        &mut file.bytes,
        64 + 2 * 32 + 24,
        &(u64::from(u32::MAX) + 1).to_le_bytes(),
    )?;
    file.section_error(3, "section length does not match count")?;
    // A pool exceeding u32 requires over 4 GiB of actual input; a forged length is rejected before allocation.
    let mut file = Encoding::tiny()?;
    let start = file.at(4, 0)?;
    let len = u64::from(u32::MAX) + 1;
    put(&mut file.bytes, 64 + 3 * 32 + 16, &len.to_le_bytes())?;
    put(&mut file.bytes, 64 + 3 * 32 + 24, &len.to_le_bytes())?;
    repair_crc(&mut file.bytes)?;
    assert!(
        matches!(persist_blocks::decode_checked(&file.bytes), Err(PersistError::Truncated { needed, available }) if needed == start + usize::try_from(len)? && available == file.bytes.len())
    );
    Ok(())
}

tiny_tree_test!(
    root_parent,
    0,
    "invalid root identity, parent, name or flags",
    |file| {
        file.node32(0, 40, 1)?;
    }
);
tiny_tree_test!(
    root_name,
    0,
    "invalid root identity, parent, name or flags",
    |file| {
        file.write(4, 0, b"x")?;
    }
);
#[test]
fn root_flags() -> TestResult {
    for flags in [0, DIRECTORY | SYNTHETIC, DIRECTORY | HARDLINK_DUP] {
        let mut file = Encoding::tiny()?;
        file.flags(0, flags)?;
        file.tree_error(0, "invalid root identity, parent, name or flags")?;
    }
    Ok(())
}
tiny_tree_test!(
    root_file_reference,
    0,
    "invalid root identity, parent, name or flags",
    |file| {
        file.reference(0, (7 << 48) | 6)?;
    }
);
tiny_tree_test!(
    synthetic_flags,
    1,
    "invalid synthetic directory metadata",
    |file| {
        file.flags(1, SYNTHETIC)?;
    }
);
tiny_tree_test!(
    synthetic_metadata,
    1,
    "invalid synthetic directory metadata",
    |file| {
        file.node64(1, 32, 1)?;
    }
);
tiny_tree_test!(
    synthetic_unknown_name,
    1,
    "unknown synthetic directory name",
    |file| {
        file.rename(1, "<unknown>")?;
    }
);
tiny_tree_test!(
    synthetic_order,
    2,
    "synthetic directories repeated or out of order",
    |file| {
        file.rename(1, "<orphaned>")?;
        file.rename(2, "<NTFS metadata>")?;
        file.flags(2, DIRECTORY | SYNTHETIC)?;
    }
);
tiny_tree_test!(
    synthetic_repeated,
    2,
    "synthetic directories repeated or out of order",
    |file| {
        file.rename(2, "<NTFS metadata>")?;
        file.flags(2, DIRECTORY | SYNTHETIC)?;
    }
);
tiny_tree_test!(
    synthetic_after_record,
    3,
    "synthetic directory follows record nodes",
    |file| {
        file.flags(3, DIRECTORY | SYNTHETIC)?;
    }
);
tiny_tree_test!(
    record_groups_descending,
    3,
    "record groups are not ascending or include root record",
    |file| {
        file.reference(2, (7 << 48) | 20)?;
    }
);
tiny_tree_test!(
    record_five_outside_root,
    3,
    "record groups are not ascending or include root record",
    |file| {
        file.reference(3, (7 << 48) | 5)?;
    }
);
tiny_tree_test!(
    group_must_start_with_primary,
    2,
    "hard-link group does not start with primary",
    |file| {
        file.toggle_flags(2, HARDLINK_DUP)?;
    }
);
tiny_tree_test!(
    record_node_without_file_reference,
    3,
    "record node has no file reference",
    |file| {
        let bitmap = file.at(7, 0)?;
        file.bytes[bitmap] &= !(1 << 3);
        file.reference(3, 0)?;
    }
);
tiny_tree_test!(
    group_multiple_primaries,
    3,
    "hard-link group has multiple primary nodes",
    |file| {
        file.reference(3, 7 << 48)?;
    }
);
tiny_tree_test!(
    group_metadata_mismatch,
    3,
    "hard-link group metadata differs",
    |file| {
        file.reference(3, 7 << 48)?;
        file.toggle_flags(3, HARDLINK_DUP)?;
    }
);
tiny_tree_test!(
    csr_child_start_partition,
    1,
    "CSR ranges do not partition edges in arena order",
    |file| {
        file.node32(1, 44, 2)?;
    }
);
tiny_tree_test!(
    csr_edge_count,
    0,
    "CSR edge count is not node count minus one",
    |file| {
        file.replace_section(5, &1u32.to_le_bytes(), 1)?;
    }
);
tiny_tree_test!(
    csr_child_len_partition,
    1,
    "CSR ranges do not partition edges in arena order",
    |file| {
        file.node32(0, 48, 0)?;
    }
);
tiny_tree_test!(
    csr_repeated_child,
    1,
    "root or repeated child in CSR",
    |file| {
        file.write(5, 4, &1u32.to_le_bytes())?;
    }
);
tiny_tree_test!(csr_parent_mismatch, 3, "CSR parent mismatch", |file| {
    file.node32(3, 40, 0)?;
});
tiny_tree_test!(
    file_with_children,
    2,
    "non-directory or duplicate node has children",
    |file| {
        file.move_child(3, 2)?;
    }
);
tiny_tree_test!(
    disconnected_cycle,
    0,
    "nodes unreachable from root",
    |file| {
        // CSR uniqueness rejects reachable cycles first; a disconnected self-cycle reaches connectivity validation.
        file.toggle_flags(2, DIRECTORY)?;
        file.set_children(&[vec![1, 3], vec![], vec![2], vec![]])?;
    }
);
tiny_tree_test!(
    name_pool_not_covered,
    0,
    "names do not cover pool",
    |file| {
        file.write(3, 3 * 64 + 60, &7u16.to_le_bytes())?;
    }
);
tiny_tree_test!(name_out_of_bounds, 3, "invalid UTF-8 name range", |file| {
    file.write(3, 3 * 64 + 60, &u16::MAX.to_le_bytes())?;
});
tiny_tree_test!(
    name_not_packed,
    3,
    "names are not tightly packed in arena order",
    |file| {
        file.node32(3, 56, 21)?;
    }
);
tiny_tree_test!(name_invalid_utf8, 3, "invalid UTF-8 name range", |file| {
    file.write(4, 20, &[0xff])?;
});
tiny_tree_test!(unknown_flag_bit, 3, "unknown node flag bits", |file| {
    file.toggle_flags(3, 1 << 8)?;
});
tiny_tree_test!(
    hidden_flag_mismatch,
    3,
    "hidden or system flag differs from attributes",
    |file| {
        file.toggle_flags(3, NodeFlags::HIDDEN.raw())?;
    }
);
tiny_tree_test!(
    system_flag_mismatch,
    3,
    "hidden or system flag differs from attributes",
    |file| {
        file.toggle_flags(3, NodeFlags::SYSTEM.raw())?;
    }
);
tiny_tree_test!(
    reparse_flag_mismatch,
    3,
    "reparse flag differs from attributes and tag",
    |file| {
        file.toggle_flags(3, NodeFlags::REPARSE.raw())?;
    }
);
tiny_tree_test!(
    directory_attribute_without_flag,
    3,
    "attribute requires missing node flag",
    |file| {
        file.write(8, 3 * 4, &0x10u32.to_le_bytes())?;
    }
);
tiny_tree_test!(
    sparse_attribute_without_flag,
    3,
    "attribute requires missing node flag",
    |file| {
        file.write(8, 3 * 4, &0x200u32.to_le_bytes())?;
    }
);
tiny_tree_test!(
    compressed_attribute_without_flag,
    3,
    "attribute requires missing node flag",
    |file| {
        file.write(8, 3 * 4, &0x800u32.to_le_bytes())?;
    }
);
tiny_tree_test!(
    aggregate_total_size,
    0,
    "subtree totals differ from self and primary children",
    |file| {
        file.node64(0, 16, 0)?;
    }
);
tiny_tree_test!(
    aggregate_total_alloc,
    0,
    "subtree totals differ from self and primary children",
    |file| {
        file.node64(0, 24, 0)?;
    }
);
tiny_tree_test!(
    aggregate_file_count,
    0,
    "subtree totals differ from self and primary children",
    |file| {
        file.node32(0, 52, 0)?;
    }
);
tiny_tree_test!(
    children_out_of_order,
    1,
    "children are not in total-allocation ranking order",
    |file| {
        file.write(5, 4, &3u32.to_le_bytes())?;
        file.write(5, 8, &2u32.to_le_bytes())?;
    }
);

#[test]
fn hardlink_duplicate_with_children() -> TestResult {
    let mut file = Encoding::basic()?;
    let count = section(&file.bytes, 3)?.2;
    let duplicate = (1..count)
        .find(|&index| {
            file.node_flags(index)
                .is_ok_and(|flags| flags & HARDLINK_DUP != 0)
        })
        .ok_or("missing hard link")?;
    let reference = u64_at(&file.bytes, file.optional_at(7, duplicate, 8)?)?;
    for index in 1..count {
        if u64_at(&file.bytes, file.optional_at(7, index, 8)?)? == reference {
            file.toggle_flags(index, DIRECTORY)?;
        }
    }
    let child = (1..count)
        .rev()
        .find(|&index| {
            index != duplicate
                && file
                    .node_flags(index)
                    .is_ok_and(|flags| flags & (DIRECTORY | SYNTHETIC | HARDLINK_DUP) == 0)
        })
        .ok_or("missing primary file")?;
    file.move_child(u32::try_from(child)?, duplicate)?;
    file.tree_error(
        u32::try_from(duplicate)?,
        "non-directory or duplicate node has children",
    )
}

const TOP_REASON: &str = "top files differ from bounded primary-file selection";

#[test]
fn top_files_wrong_member() -> TestResult {
    let mut file = Encoding::tiny()?;
    file.write(11, 0, &0u32.to_le_bytes())?;
    file.index_error(TOP_REASON)
}

#[test]
fn top_files_wrong_order() -> TestResult {
    let mut file = Encoding::tiny()?;
    file.write(11, 0, &3u32.to_le_bytes())?;
    file.write(11, 4, &2u32.to_le_bytes())?;
    file.index_error(TOP_REASON)
}

#[test]
fn top_files_wrong_length() -> TestResult {
    let mut file = Encoding::tiny()?;
    file.replace_section(11, &2u32.to_le_bytes(), 1)?;
    file.index_error(TOP_REASON)
}

const EXT_TOTALS: &str = "extension summary differs from primary-file totals";

#[test]
fn ext_stats_wrong_totals() -> TestResult {
    for field in [0, 8, 16] {
        let mut file = Encoding::tiny()?;
        let value = u64_at(&file.bytes, file.at(12, field)?)? + 1;
        file.write(12, field, &value.to_le_bytes())?;
        file.index_error(EXT_TOTALS)?;
    }
    Ok(())
}

#[test]
fn ext_stats_missing_group() -> TestResult {
    let mut file = Encoding::tiny()?;
    let at = file.at(12, 0)?;
    let first = file
        .bytes
        .get(at..at + 32)
        .ok_or("missing extension")?
        .to_vec();
    file.replace_section(12, &first, 1)?;
    file.replace_section(13, &[], 0)?;
    file.index_error("missing or noncanonical extension summary")
}

#[test]
fn ext_stats_duplicate() -> TestResult {
    let mut file = Encoding::tiny()?;
    // Unequal allocations pass the earlier ordering check so duplicate detection is exercised.
    file.write(12, 32 + 28, &0u32.to_le_bytes())?;
    file.replace_section(13, &[], 0)?;
    file.index_error("duplicate extension summary")
}

#[test]
fn ext_stats_wrong_order() -> TestResult {
    let mut file = Encoding::tiny()?;
    file.write(12, 32 + 16, &16385u64.to_le_bytes())?;
    file.index_error("extension summaries are not in allocation and name order")
}

#[test]
fn ext_stats_zero_count() -> TestResult {
    let mut file = Encoding::tiny()?;
    file.write(12, 0, &0u64.to_le_bytes())?;
    file.index_error(EXT_TOTALS)
}

fn proptest_config() -> Config {
    let mut config = Config::default();
    if std::env::var_os("PROPTEST_CASES").is_none() {
        config.cases = 32;
    }
    config
}

proptest! {
    #![proptest_config(proptest_config())]
    #[test]
    fn repaired_random_byte_flips_are_rejected_or_canonical(
        flips in prop::collection::vec((any::<usize>(), 1u8..=255), 1..=8),
    ) {
        let mut file = Encoding::tiny().map_err(|error| TestCaseError::fail(error.to_string()))?;
        for (position, mask) in flips {
            let position = position % file.bytes.len();
            file.bytes[position] ^= mask;
        }
        repair_crc(&mut file.bytes).map_err(|error| TestCaseError::fail(error.to_string()))?;
        if let Ok(decoded) = persist_blocks::decode_checked(&file.bytes) {
            let encoded = decoded.to_bytes().map_err(|error| TestCaseError::fail(error.to_string()))?;
            prop_assert_eq!(encoded, file.bytes);
        }
    }
}

#[test]
fn extension_utf8_error_precedes_later_name_table_errors() -> TestResult {
    for later_length in [2u32, u32::MAX] {
        let mut file = Encoding::tiny()?;
        // The first group consumes one byte; the next group has an invalid offset or length.
        file.write(12, 28, &1u32.to_le_bytes())?;
        let later_offset: u32 = if later_length == 2 { 2 } else { 1 };
        file.write(12, 32 + 24, &later_offset.to_le_bytes())?;
        file.write(12, 32 + 28, &later_length.to_le_bytes())?;
        file.write(13, 0, &[0xff])?;
        repair_crc(&mut file.bytes)?;
        assert!(matches!(
            persist_blocks::decode_checked(&file.bytes),
            Err(PersistError::Utf8 { section: 13, .. })
        ));
    }
    Ok(())
}

#[test]
fn checksum_error_precedes_volume_value_errors() -> TestResult {
    let mut file = Encoding::tiny()?;
    file.write(1, 0, &513u16.to_le_bytes())?;
    assert!(matches!(
        persist_blocks::decode_checked(&file.bytes),
        Err(PersistError::ChecksumMismatch { .. })
    ));
    Ok(())
}
