//! Canonical v1 persistence through the public scan API.

#[path = "support/persist_blocks.rs"]
mod persist_blocks;
mod support;

use adamite_core::{
    scan::{ScanContext, ScanOutput, ScanStats},
    snapshot::{NodeId, PersistError},
};
use adamite_fixtures::Fixture;
use std::path::Path;
use support::{TestResult, scan, section, tiny_image, u32_at, u64_at};

fn counters(stats: &ScanStats) -> [u64; 26] {
    stats.counters().map(|(_, value)| value)
}

fn check_layout(output: &ScanOutput) -> TestResult {
    let bytes = output.to_bytes()?;
    assert_eq!(bytes, output.to_bytes()?);
    let mut encoder = output.encoder()?;
    let mut chunks = Vec::new();
    while let Some(chunk) = encoder.next_chunk()? {
        assert!(!chunk.is_empty());
        assert!(chunk.len() <= adamite_core::snapshot::SnapshotEncoder::CHUNK_SIZE);
        chunks.extend_from_slice(chunk);
    }
    let (offset, crc) = encoder.checksum_patch().ok_or("unfinished checksum")?;
    chunks[offset..offset + 4].copy_from_slice(&crc);
    assert_eq!(chunks, bytes);
    assert_eq!(persist_blocks::decode_checked(&bytes)?, *output);
    let decoded = ScanOutput::from_bytes(&bytes)?;
    assert_eq!(&decoded, output);
    assert_eq!(decoded.to_bytes()?, bytes);
    assert_eq!(&bytes[..8], b"ADAMITE\0");
    assert_eq!(u32_at(&bytes, 8)?, 1);
    assert_eq!(u32_at(&bytes, 12)?, 64);
    assert_eq!(u64_at(&bytes, 16)?, u64::try_from(bytes.len())?);
    assert_eq!(u32_at(&bytes, 24)?, 13);
    assert_eq!(u32_at(&bytes, 28)?, 0);
    assert!(bytes[36..64].iter().all(|&byte| byte == 0));
    let mut zeroed = bytes.clone();
    zeroed[32..36].fill(0);
    assert_eq!(u32_at(&bytes, 32)?, crc32fast::hash(&zeroed));

    let snapshot = &output.snapshot;
    let n = snapshot.len();
    let mut end = 480;
    for kind in 1..=13 {
        let entry = 64 + (kind - 1) * 32;
        assert_eq!(u32_at(&bytes, entry)?, u32::try_from(kind)?);
        assert_eq!(u32_at(&bytes, entry + 4)?, 0);
        let (offset, len, count) = section(&bytes, kind)?;
        assert_eq!(offset, (end + 7) & !7);
        assert!(bytes[end..offset].iter().all(|&byte| byte == 0));
        end = offset + len;
        match kind {
            1 => {
                let context = snapshot.volume().context();
                assert_eq!(count, 1);
                let label = context.label.as_deref().unwrap_or("");
                assert_eq!(len, 69 + label.len());
                let fields = snapshot.volume().geometry().fields();
                assert_eq!(
                    &bytes[offset..offset + 2],
                    &fields.bytes_per_sector.to_le_bytes()
                );
                for (relative, value) in [
                    (2, fields.sectors_per_cluster),
                    (6, fields.file_record_size),
                    (10, fields.index_record_size),
                ] {
                    assert_eq!(u32_at(&bytes, offset + relative)?, value);
                }
                for (relative, value) in [
                    (14, fields.total_sectors),
                    (22, fields.mft_lcn),
                    (30, fields.mftmirr_lcn),
                    (38, fields.serial),
                ] {
                    assert_eq!(u64_at(&bytes, offset + relative)?, value);
                }
                for (relative, value) in
                    [(46, context.scanned_at_filetime), (55, context.free_bytes)]
                {
                    assert_eq!(bytes[offset + relative], u8::from(value.is_some()));
                    assert_eq!(u64_at(&bytes, offset + relative + 1)?, value.unwrap_or(0));
                }
                assert_eq!(bytes[offset + 64], u8::from(context.label.is_some()));
                assert_eq!(u32_at(&bytes, offset + 65)?, u32::try_from(label.len())?);
                assert_eq!(&bytes[offset + 69..end], label.as_bytes());
            }
            2 => {
                assert_eq!((len, count), (208, 26));
                let expected: Vec<u8> = counters(&output.stats)
                    .into_iter()
                    .flat_map(u64::to_le_bytes)
                    .collect();
                assert_eq!(&bytes[offset..end], expected);
            }
            3 => assert_eq!((len, count), (n * 64, n)),
            4 | 13 => assert_eq!(len, count),
            5 => assert_eq!((len, count), ((n - 1) * 4, n - 1)),
            6 => assert_eq!((len, count), (n * 8, n)),
            7 | 10 => {
                let width = if kind == 7 { 8 } else { 4 };
                let bitmap_len = n.div_ceil(8);
                let values_start = (bitmap_len + 7) & !7;
                assert_eq!((len, count), (values_start + n * width, n));
                assert!(
                    bytes[offset + bitmap_len..offset + values_start]
                        .iter()
                        .all(|&byte| byte == 0)
                );
                if !n.is_multiple_of(8) {
                    assert_eq!(bytes[offset + bitmap_len - 1] >> (n % 8), 0);
                }
                for index in 0..n {
                    let node = snapshot
                        .node(NodeId::from_raw(u32::try_from(index)?))
                        .ok_or("missing node")?;
                    let value = if kind == 7 {
                        node.file_ref().map(|value| value.raw())
                    } else {
                        node.reparse_tag().map(u64::from)
                    };
                    assert_eq!(
                        (bytes[offset + index / 8] >> (index % 8)) & 1,
                        u8::from(value.is_some())
                    );
                    let at = offset + values_start + index * width;
                    let actual = if kind == 7 {
                        u64_at(&bytes, at)?
                    } else {
                        u64::from(u32_at(&bytes, at)?)
                    };
                    assert_eq!(actual, value.unwrap_or(0));
                }
            }
            8 => assert_eq!((len, count), (n * 4, n)),
            9 => assert_eq!((len, count), (n * 2, n)),
            11 => assert_eq!(
                (len, count),
                (snapshot.top_files().len() * 4, snapshot.top_files().len())
            ),
            12 => {
                assert_eq!(
                    (len, count),
                    (snapshot.ext_stats().len() * 32, snapshot.ext_stats().len())
                );
                let (pool, pool_len, _) = section(&bytes, 13)?;
                let mut name_offset = 0;
                for (index, group) in snapshot.ext_stats().iter().enumerate() {
                    let at = offset + index * 32;
                    assert_eq!(u64_at(&bytes, at)?, group.count());
                    assert_eq!(u64_at(&bytes, at + 8)?, group.total_size());
                    assert_eq!(u64_at(&bytes, at + 16)?, group.total_alloc());
                    assert_eq!(usize::try_from(u32_at(&bytes, at + 24)?)?, name_offset);
                    assert_eq!(
                        usize::try_from(u32_at(&bytes, at + 28)?)?,
                        group.extension().len()
                    );
                    assert_eq!(
                        &bytes[pool + name_offset..pool + name_offset + group.extension().len()],
                        group.extension().as_bytes()
                    );
                    name_offset += group.extension().len();
                }
                assert_eq!(name_offset, pool_len);
            }
            _ => return Err("unknown kind".into()),
        }
    }
    assert_eq!(bytes.len(), end);
    Ok(())
}

#[test]
fn fixture_encodings_are_deterministic_and_follow_v1_layout() -> TestResult {
    for name in ["basic-4k", "frag-512", "4kn-64k"] {
        let fixture = Fixture::load(
            &Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(format!("../../tests/fixtures/ntfs/{name}.json")),
        )?;
        let output = scan(
            fixture.image(),
            ScanContext {
                label: Some("保存用 😀".to_owned()),
                scanned_at_filetime: Some(0x1234_5678_9abc_def0),
                free_bytes: Some(0),
            },
        )?;
        check_layout(&output)?;
    }
    Ok(())
}

#[test]
fn absent_and_present_empty_context_payloads() -> TestResult {
    for label in [None, Some(String::new())] {
        check_layout(&scan(
            &tiny_image(),
            ScanContext {
                label,
                ..ScanContext::default()
            },
        )?)?;
    }
    Ok(())
}

#[test]
fn stats_counters_keep_the_v1_order() -> TestResult {
    let mut output = scan(&tiny_image(), ScanContext::default())?;
    let stats = &mut output.stats;
    let fields = [
        &mut stats.records_total,
        &mut stats.records_in_use,
        &mut stats.base_records,
        &mut stats.extension_records,
        &mut stats.unused,
        &mut stats.deleted,
        &mut stats.baad,
        &mut stats.corrupt_records,
        &mut stats.bad_fixup,
        &mut stats.invalid_fixup_metadata,
        &mut stats.invalid_signature,
        &mut stats.corrupt_headers,
        &mut stats.corrupt_attributes,
        &mut stats.type_storage_mismatches,
        &mut stats.corrupt_values,
        &mut stats.corrupt_runlists,
        &mut stats.out_of_volume_extents,
        &mut stats.invalid_attribute_lists,
        &mut stats.invalid_file_refs,
        &mut stats.invalid_utf16,
        &mut stats.rejected_extensions,
        &mut stats.corrupt_streams,
        &mut stats.orphans,
        &mut stats.cycle_nodes,
        &mut stats.hardlink_duplicates,
        &mut stats.metafiles,
    ];
    for (index, field) in fields.into_iter().enumerate() {
        *field = 0x0102_0304_0506_0700 | u64::try_from(index)?;
    }
    let bytes = output.to_bytes()?;
    let (offset, len, count) = section(&bytes, 2)?;
    assert_eq!((len, count), (208, 26));
    for index in 0..26 {
        assert_eq!(
            u64_at(&bytes, offset + index * 8)?,
            0x0102_0304_0506_0700 | u64::try_from(index)?
        );
    }
    assert_eq!(ScanOutput::from_bytes(&bytes)?, output);
    Ok(())
}

#[test]
fn tiny_scan_has_golden_v1_encoding() -> TestResult {
    let output = scan(
        &tiny_image(),
        ScanContext {
            label: Some("tiny".to_owned()),
            scanned_at_filetime: Some(123),
            free_bytes: Some(456),
        },
    )?;
    check_layout(&output)?;
    let bytes = output.to_bytes()?;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    let expected = concat!(
        "4144414d495445000100000040000000f3040000000000000d0000000000000017dc26d3000000000000000000000000",
        "000000000000000000000000000000000100000000000000e00100000000000049000000000000000100000000000000",
        "02000000000000003002000000000000d0000000000000001a0000000000000003000000000000000003000000000000",
        "00010000000000000400000000000000040000000000000000040000000000001c000000000000001c00000000000000",
        "050000000000000020040000000000000c00000000000000030000000000000006000000000000003004000000000000",
        "200000000000000004000000000000000700000000000000500400000000000028000000000000000400000000000000",
        "080000000000000078040000000000001000000000000000040000000000000009000000000000008804000000000000",
        "080000000000000004000000000000000a00000000000000900400000000000018000000000000000400000000000000",
        "0b00000000000000a804000000000000080000000000000002000000000000000c00000000000000b004000000000000",
        "400000000000000002000000000000000d00000000000000f00400000000000003000000000000000300000000000000",
        "0002010000000004000000100000800000000000000004000000000000004000000000000000efcdab8967452301017b",
        "0000000000000001c801000000000000010400000074696e790000000000000010000000000000000300000000000000",
        "030000000000000000000000000000000d00000000000000000000000000000000000000000000000000000000000000",
        "000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
        "000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
        "000000000000000000000000000000000000000000000000000000000000000000000000000000000200000000000000",
        "000000000000000000000000000000000240000000000000004000000000000000000000000000000000000000000000",
        "010000000200000000000000010001000000000000000000000000000000000002400000000000000040000000000000",
        "000000000000000000000000010000000200000002000000010000000f00810000400000000000000040000000000000",
        "004000000000000000400000000000000000000000000000010000000300000000000000010000001000000004000000",
        "020000000000000000000000000000000200000000000000000000000000000000000000000000000100000003000000",
        "000000000100000014000000080000002f3c4e544653206d657461646174613e244d46546e6f74652e54785400000000",
        "010000000200000003000000000000000000000000000000000000000000000000000000000000000000000000000000",
        "0d000000000000000500000000000700000000000000000000000000000007000c000000000007000000000000000000",
        "000000000000000001000000010001000000000000000000000000000000000000000000000000000200000003000000",
        "010000000000000000400000000000000040000000000000000000000000000001000000000000000200000000000000",
        "00000000000000000000000003000000747874",
    );
    assert_eq!(hex, expected);
    let golden = expected
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| -> TestResult<u8> { Ok(u8::from_str_radix(std::str::from_utf8(pair)?, 16)?) })
        .collect::<TestResult<Vec<u8>>>()?;
    let decoded = ScanOutput::from_bytes(&golden)?;
    assert_eq!(decoded, output);
    assert_eq!(decoded.to_bytes()?, golden);
    Ok(())
}

#[test]
fn decoder_rejects_bad_magic_truncation_and_checksum_mismatch() -> TestResult {
    let bytes = scan(&tiny_image(), ScanContext::default())?.to_bytes()?;
    let mut bad_magic = bytes.clone();
    bad_magic[0] ^= 1;
    assert!(matches!(
        ScanOutput::from_bytes(&bad_magic),
        Err(PersistError::BadMagic)
    ));
    assert!(matches!(
        ScanOutput::from_bytes(&bytes[..63]),
        Err(PersistError::Truncated { .. })
    ));
    assert!(matches!(
        ScanOutput::from_bytes(&bytes[..bytes.len() - 1]),
        Err(PersistError::Truncated { .. })
    ));
    let mut bad_checksum = bytes;
    bad_checksum[32] ^= 1;
    assert!(matches!(
        ScanOutput::from_bytes(&bad_checksum),
        Err(PersistError::ChecksumMismatch { .. })
    ));
    Ok(())
}

#[test]
fn encoder_crosses_scratch_boundaries_and_borrows_large_labels() -> TestResult {
    use adamite_fixtures::{SyntheticConfig, SyntheticVolume};
    let volume = SyntheticVolume::new(SyntheticConfig::reference(20_002)?)?;
    let mut image = std::io::Cursor::new(Vec::new());
    volume.write(&mut image)?;
    let output = scan(
        image.get_ref(),
        ScanContext {
            label: Some("a".repeat((1 << 20) + 7)),
            ..ScanContext::default()
        },
    )?;
    check_layout(&output)
}
