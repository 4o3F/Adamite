#![allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

//! FILE record headers, atomic fixup, and classification precedence.

mod support;

use adamite_fixtures::builders::RecordBuilder;
use adamite_ntfs::fixup::{self, FixupError};
use adamite_ntfs::{FileRecord, RecordClass, RecordError, RecordFlags, RecordHeader, classify};
use proptest::prelude::*;

#[test]
fn peek_decodes_every_fixed_field_without_mutation() {
    let builder = RecordBuilder::default();
    let bytes = builder.build();
    let original = bytes.clone();
    let header = RecordHeader::peek(&bytes).unwrap();
    assert_eq!(header.signature(), *b"FILE");
    assert_eq!(header.usa_offset(), 0x30);
    assert_eq!(header.usa_count(), 3);
    assert_eq!(header.lsn(), builder.lsn);
    assert_eq!(header.sequence(), builder.sequence);
    assert_eq!(header.hard_links(), builder.hard_links);
    assert_eq!(header.first_attribute_offset(), 56);
    assert_eq!(header.flags().raw(), 1);
    assert!(header.flags().contains(RecordFlags::IN_USE));
    assert!(!header.flags().contains(RecordFlags::DIRECTORY));
    assert_eq!(header.bytes_used(), 64);
    assert_eq!(header.bytes_allocated(), 1024);
    assert!(header.base_reference().is_null());
    assert_eq!(header.next_attribute_id(), builder.next_attribute_id);
    assert_eq!(header.record_number(), Some(42));
    assert_eq!(bytes, original);
}

#[test]
fn peek_checks_layout_specific_truncation() {
    for usa_offset in [0x2a, 0x30] {
        let bytes = RecordBuilder {
            usa_offset,
            ..Default::default()
        }
        .build();
        let minimum = if usa_offset == 0x30 { 0x30 } else { 0x2a };
        for length in 0..minimum {
            assert_eq!(
                RecordHeader::peek(&bytes[..length]),
                Err(RecordError::TruncatedHeader)
            );
        }
        assert!(RecordHeader::peek(&bytes[..minimum]).is_ok());
    }
}

#[test]
fn unknown_flags_and_zero_counters_are_kept() {
    let bytes = RecordBuilder {
        flags: 0x0105,
        sequence: 0,
        hard_links: 0,
        ..Default::default()
    }
    .build();
    let header = RecordHeader::peek(&bytes).unwrap();
    assert_eq!(header.flags().raw(), 0x0105);
    assert!(header.flags().contains(RecordFlags::IN_USE));
    assert!(!header.flags().contains(RecordFlags::DIRECTORY));
    assert_eq!(header.sequence(), 0);
    assert_eq!(header.hard_links(), 0);
}

proptest! {
    #![proptest_config(support::proptest_config("proptest-regressions/file_record.txt"))]
    #[test]
    fn ntfs30_never_interprets_record_number_bytes(sentinel in any::<[u8; 4]>()) {
        let mut bytes = RecordBuilder { usa_offset: 0x2a, ..Default::default() }.build();
        let before = RecordHeader::peek(&bytes).unwrap();
        bytes[0x2c..0x30].copy_from_slice(&sentinel);
        prop_assert_eq!(RecordHeader::peek(&bytes).unwrap(), before);
        prop_assert_eq!(before.record_number(), None);
    }
}

#[test]
fn classification_scenarios_and_header_validation() {
    for fill in [0, 0xff] {
        let mut bytes = vec![fill; 1024];
        let before = bytes.clone();
        assert!(matches!(classify(&mut bytes), RecordClass::Unused));
        assert_eq!(bytes, before);
    }
    let mut baad = vec![0xa5; 1024];
    baad[..4].copy_from_slice(b"BAAD");
    let before = baad.clone();
    assert!(matches!(classify(&mut baad), RecordClass::Baad));
    assert_eq!(baad, before);
    let mut invalid = vec![0; 513];
    invalid[..4].copy_from_slice(b"BAAD");
    assert!(matches!(
        classify(&mut invalid),
        RecordClass::Corrupt(RecordError::InvalidRecordSize { .. })
    ));
    for base in [0, (7u64 << 48) | 1234, 3u64 << 48] {
        let builder = RecordBuilder {
            base,
            ..Default::default()
        };
        let mut bytes = builder.build();
        let record = match classify(&mut bytes) {
            RecordClass::Base(record) if base == 0 => record,
            RecordClass::Extension(record) if base != 0 => record,
            other => panic!("unexpected class {other:?}"),
        };
        assert_eq!(record.header().base_reference().raw(), base);
        assert_eq!(&record.as_bytes()[510..512], &[0, 0]);
    }
    let builder = RecordBuilder {
        flags: 0,
        ..Default::default()
    };
    let mut deleted = builder.build();
    builder.corrupt_tail(&mut deleted, 1);
    deleted[6..8].copy_from_slice(&u16::MAX.to_le_bytes());
    let before = deleted.clone();
    assert!(
        matches!(classify(&mut deleted), RecordClass::NotInUse(header) if header.flags().raw() == 0)
    );
    assert_eq!(deleted, before);
    let mut unknown = RecordBuilder::default().build();
    unknown[..4].copy_from_slice(b"WHAT");
    let before = unknown.clone();
    assert!(matches!(
        classify(&mut unknown),
        RecordClass::Corrupt(RecordError::InvalidSignature { .. })
    ));
    assert_eq!(unknown, before);
    let builder = RecordBuilder::default();
    let mut torn = builder.build();
    builder.corrupt_tail(&mut torn, 1);
    let before = torn.clone();
    assert!(matches!(
        classify(&mut torn),
        RecordClass::Corrupt(RecordError::Fixup {
            source: FixupError::TrailerMismatch { stride: 1, .. }
        })
    ));
    assert_eq!(torn, before);

    let mut valid = RecordBuilder::default().build();
    valid[24..28].copy_from_slice(&416u32.to_le_bytes());
    let record = FileRecord::parse(fixup::apply(&mut valid).unwrap()).unwrap();
    assert_eq!(record.header().bytes_used(), 416);

    let cases = [
        (
            28,
            999u32.to_le_bytes().to_vec(),
            RecordError::InvalidAllocatedSize {
                declared: 999,
                actual: 1024,
            },
        ),
        (
            24,
            1032u32.to_le_bytes().to_vec(),
            RecordError::InvalidUsedSize {
                used: 1032,
                allocated: 1024,
            },
        ),
        (
            24,
            63u32.to_le_bytes().to_vec(),
            RecordError::InvalidUsedSize {
                used: 63,
                allocated: 1024,
            },
        ),
        (
            20,
            57u16.to_le_bytes().to_vec(),
            RecordError::InvalidFirstAttribute {
                offset: 57,
                used: 64,
            },
        ),
        (
            20,
            64u16.to_le_bytes().to_vec(),
            RecordError::InvalidFirstAttribute {
                offset: 64,
                used: 64,
            },
        ),
        (
            4,
            0x2bu16.to_le_bytes().to_vec(),
            RecordError::InvalidUsaOffset { offset: 0x2b },
        ),
        (
            4,
            0x28u16.to_le_bytes().to_vec(),
            RecordError::InvalidUsaOffset { offset: 0x28 },
        ),
    ];
    for (offset, value, expected) in cases {
        let mut bytes = RecordBuilder::default().build();
        bytes[offset..offset + value.len()].copy_from_slice(&value);
        let before = bytes.clone();
        assert!(matches!(classify(&mut bytes), RecordClass::Corrupt(error) if error == expected));
        assert_eq!(bytes, before);
        if !matches!(expected, RecordError::InvalidUsaOffset { .. }) {
            assert_eq!(
                FileRecord::parse(fixup::apply(&mut bytes).unwrap()).unwrap_err(),
                expected
            );
        }
    }
}

proptest! {
    #![proptest_config(support::proptest_config("proptest-regressions/file_record.txt"))]
    #[test]
    fn peek_matches_fixed_header(flags in any::<u16>(), base in any::<u64>(), sequence in any::<u16>(), links in any::<u16>(), layout31 in any::<bool>()) {
        let mut bytes = RecordBuilder { flags, base, sequence, hard_links: links, usa_offset: if layout31 { 0x30 } else { 0x2a }, ..Default::default() }.build();
        let before = RecordHeader::peek(&bytes).unwrap();
        let record = FileRecord::parse(fixup::apply(&mut bytes).unwrap()).unwrap();
        prop_assert_eq!(record.header(), before);
    }

    #[test]
    fn classification_matches_independent_precedence_model(
        signature in prop::sample::select(vec![*b"FILE", *b"BAAD", *b"WHAT", [0; 4], [0xff; 4]]),
        fill in 0u8..3, flags in any::<u16>(), base in prop_oneof![Just(0u64), Just(3u64 << 48), any::<u64>()],
        torn in any::<bool>(), invalid_header in any::<bool>(), layout31 in any::<bool>(),
        size in prop::sample::select(vec![512usize, 1024, 4096, 65536]),
    ) {
        let builder = RecordBuilder { size, flags, base, usa_offset: if layout31 { 0x30 } else { 0x2a }, ..Default::default() };
        let mut bytes = match fill { 0 => vec![0; size], 1 => vec![0xff; size], _ => builder.build() };
        if fill == 2 {
            bytes[..4].copy_from_slice(&signature);
            if torn { builder.corrupt_tail(&mut bytes, size / 512 - 1); }
            if invalid_header { bytes[24..28].copy_from_slice(&(size as u32 + 8).to_le_bytes()); }
        }
        let before = bytes.clone();
        let expected = if fill < 2 { 0 } else if signature == *b"BAAD" { 1 }
            else if signature != *b"FILE" || invalid_header { 5 }
            else if flags & 1 == 0 { 2 } else if torn { 5 } else if base == 0 { 3 } else { 4 };
        let actual = match classify(&mut bytes) {
            RecordClass::Unused => 0, RecordClass::Baad => 1,
            RecordClass::NotInUse(header) => { prop_assert_eq!(header.flags().raw(), flags); 2 },
            RecordClass::Base(record) => { prop_assert!(record.header().base_reference().is_null()); 3 },
            RecordClass::Extension(record) => { prop_assert_eq!(record.header().base_reference().raw(), base); 4 },
            RecordClass::Corrupt(_) => 5, other => panic!("unexpected class {other:?}"),
        };
        prop_assert_eq!(actual, expected);
        if !matches!(actual, 3 | 4) { prop_assert_eq!(bytes, before); }
    }

    #[test]
    fn ntfs30_sentinel_is_ignored_after_classification(sentinel in any::<[u8; 4]>()) {
        let builder = RecordBuilder { usa_offset: 0x2a, ..Default::default() };
        let mut bytes = builder.build();
        let header = RecordHeader::peek(&bytes).unwrap();
        bytes[0x2c..0x30].copy_from_slice(&sentinel);
        match classify(&mut bytes) {
            RecordClass::Base(record) => prop_assert_eq!(record.header(), header),
            other => prop_assert!(false, "unexpected class {other:?}"),
        }
    }

    #[test]
    fn arbitrary_record_operations_are_total_and_errors_are_atomic(input in prop::collection::vec(any::<u8>(), 0..=65536)) {
        let _ = RecordHeader::peek(&input);
        let mut fixed = input.clone();
        if fixup::apply(&mut fixed).is_err() { prop_assert_eq!(fixed, input.clone()); }
        let mut classified = input.clone();
        match classify(&mut classified) {
            RecordClass::Base(record) | RecordClass::Extension(record) => { let _ = record.header(); let _ = record.as_bytes(); },
            _ => prop_assert_eq!(classified, input),
        }
    }
}

#[test]
fn fixup_restores_only_trailers_on_all_supported_sizes_and_usns() {
    for size in [512, 1024, 4096, 65536] {
        for usn in [0, 1, 0xffff, 0xa55a] {
            let builder = RecordBuilder {
                size,
                usn,
                ..Default::default()
            };
            let mut plain = builder.plain();
            for stride in 0..size / 512 {
                let tail = (stride + 1) * 512 - 2;
                plain[tail..tail + 2].copy_from_slice(&(stride as u16 ^ usn).to_le_bytes());
            }
            let mut protected = builder.protect(&plain);
            let original = protected.clone();
            let token = fixup::apply(&mut protected).unwrap();
            for index in 0..size {
                let trailer = index % 512 >= 510;
                assert_eq!(
                    token.as_bytes()[index],
                    if trailer {
                        plain[index]
                    } else {
                        original[index]
                    }
                );
            }
        }
    }
}

#[test]
fn every_torn_tail_reports_its_stride_without_any_writes() {
    for size in [512, 1024, 4096, 65536] {
        let builder = RecordBuilder {
            size,
            ..Default::default()
        };
        for stride in 0..size / 512 {
            let mut bytes = builder.build();
            builder.corrupt_tail(&mut bytes, stride);
            let before = bytes.clone();
            assert!(
                matches!(fixup::apply(&mut bytes), Err(FixupError::TrailerMismatch { stride: index, .. }) if index == stride as u16)
            );
            assert_eq!(bytes, before);
        }
    }
}

#[test]
fn fixup_metadata_boundaries_and_validation_order() {
    for size in [0, 1, 511, 513, 1000, 65537, 131072] {
        let mut bytes = vec![0; size];
        assert!(matches!(
            fixup::apply(&mut bytes),
            Err(FixupError::InvalidRecordSize { .. })
        ));
    }
    for size in [512, 1024, 4096, 65536] {
        let builder = RecordBuilder {
            size,
            ..Default::default()
        };
        let expected = (size / 512 + 1) as u16;
        for count in [0, expected - 1, expected + 1, u16::MAX] {
            let mut bytes = builder.build();
            bytes[6..8].copy_from_slice(&count.to_le_bytes());
            let before = bytes.clone();
            assert!(matches!(
                fixup::apply(&mut bytes),
                Err(FixupError::InvalidCount { .. })
            ));
            assert_eq!(bytes, before);
        }
        for offset in [
            0,
            0x29,
            0x2a,
            0x2b,
            0x30,
            510 - expected * 2,
            512 - expected * 2,
            510,
            511,
            u16::MAX,
        ] {
            for first in [0x2a, 0x30, 56, 510, 512] {
                let mut bytes = builder.build();
                bytes[4..6].copy_from_slice(&offset.to_le_bytes());
                bytes[20..22].copy_from_slice(&(first as u16).to_le_bytes());
                let end = u128::from(offset) + u128::from(expected) * 2;
                let valid = offset >= 0x2a && offset % 2 == 0 && end <= 510 && end <= first as u128;
                if valid {
                    bytes[usize::from(offset)..end as usize].fill(0);
                    for stride in 0..size / 512 {
                        bytes[(stride + 1) * 512 - 2..(stride + 1) * 512].fill(0);
                    }
                }
                let before = bytes.clone();
                assert_eq!(
                    fixup::apply(&mut bytes).is_ok(),
                    valid,
                    "size={size}, offset={offset}, first={first}"
                );
                if !valid {
                    assert_eq!(bytes, before);
                }
            }
        }
    }
}

proptest! {
    #![proptest_config(support::proptest_config("proptest-regressions/file_record.txt"))]
    #[test]
    fn protected_records_round_trip_and_errors_are_atomic(
        size in prop::sample::select(vec![512usize, 1024, 4096, 65536]),
        usn in any::<u16>(), tails in prop::collection::vec(any::<u16>(), 128),
        selected in any::<usize>(), usa30 in any::<bool>(),
    ) {
        let builder = RecordBuilder { size, usn, usa_offset: if usa30 { 0x30 } else { 0x2a }, ..Default::default() };
        let mut plain = builder.plain();
        for (stride, tail) in tails.iter().take(size / 512).enumerate() {
            plain[(stride + 1) * 512 - 2..(stride + 1) * 512].copy_from_slice(&tail.to_le_bytes());
        }
        let mut bytes = builder.protect(&plain);
        let usa_end = usize::from(builder.usa_offset) + (size / 512 + 1) * 2;
        let token = fixup::apply(&mut bytes).unwrap();
        prop_assert_eq!(&token.as_bytes()[..usize::from(builder.usa_offset)], &plain[..usize::from(builder.usa_offset)]);
        prop_assert_eq!(&token.as_bytes()[usa_end..], &plain[usa_end..]);
        let mut torn = builder.protect(&plain);
        builder.corrupt_tail(&mut torn, selected % (size / 512));
        let before = torn.clone();
        prop_assert!(fixup::apply(&mut torn).is_err());
        prop_assert_eq!(torn, before);
    }
}
