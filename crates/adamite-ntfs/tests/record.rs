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
use adamite_ntfs::record::fixup::FixupError;
use adamite_ntfs::record::{FileRecord, RecordClass, RecordError, RecordFlags, RecordHeader};
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
        assert!(matches!(
            RecordClass::classify(&mut bytes),
            RecordClass::Unused
        ));
        assert_eq!(bytes, before);
    }
    let mut baad = vec![0xa5; 1024];
    baad[..4].copy_from_slice(b"BAAD");
    let before = baad.clone();
    assert!(matches!(
        RecordClass::classify(&mut baad),
        RecordClass::Baad
    ));
    assert_eq!(baad, before);
    let mut invalid = vec![0; 513];
    invalid[..4].copy_from_slice(b"BAAD");
    assert!(matches!(
        RecordClass::classify(&mut invalid),
        RecordClass::Corrupt(RecordError::InvalidRecordSize { .. })
    ));
    for base in [0, (7u64 << 48) | 1234, 3u64 << 48] {
        let builder = RecordBuilder {
            base,
            ..Default::default()
        };
        let mut bytes = builder.build();
        let record = match RecordClass::classify(&mut bytes) {
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
        matches!(RecordClass::classify(&mut deleted), RecordClass::NotInUse(header) if header.flags().raw() == 0)
    );
    assert_eq!(deleted, before);
    let mut unknown = RecordBuilder::default().build();
    unknown[..4].copy_from_slice(b"WHAT");
    let before = unknown.clone();
    assert!(matches!(
        RecordClass::classify(&mut unknown),
        RecordClass::Corrupt(RecordError::InvalidSignature { .. })
    ));
    assert_eq!(unknown, before);
    let builder = RecordBuilder::default();
    let mut torn = builder.build();
    builder.corrupt_tail(&mut torn, 1);
    let before = torn.clone();
    assert!(matches!(
        RecordClass::classify(&mut torn),
        RecordClass::Corrupt(RecordError::Fixup {
            source: FixupError::TrailerMismatch { stride: 1, .. }
        })
    ));
    assert_eq!(torn, before);

    let mut valid = RecordBuilder::default().build();
    valid[24..28].copy_from_slice(&416u32.to_le_bytes());
    let record = FileRecord::parse(&mut valid).unwrap();
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
        assert!(
            matches!(RecordClass::classify(&mut bytes), RecordClass::Corrupt(error) if error == expected)
        );
        assert_eq!(bytes, before);
        assert_eq!(FileRecord::parse(&mut bytes).unwrap_err(), expected);
        assert_eq!(bytes, before);
    }
}

#[test]
fn parse_errors_are_atomic_and_headers_precede_fixup() {
    use std::error::Error;

    for invalid_header in [false, true] {
        for torn in [false, true] {
            let builder = RecordBuilder::default();
            let mut bytes = builder.build();
            if invalid_header {
                bytes[28..32].copy_from_slice(&999u32.to_le_bytes());
            }
            if torn {
                builder.corrupt_tail(&mut bytes, 1);
            }
            let before = bytes.clone();
            let parsed = FileRecord::parse(&mut bytes);
            if invalid_header {
                let error = parsed.unwrap_err();
                assert_eq!(
                    error,
                    RecordError::InvalidAllocatedSize {
                        declared: 999,
                        actual: 1024
                    }
                );
                assert!(error.source().is_none());
                assert_eq!(bytes, before);
            } else if torn {
                let error = parsed.unwrap_err();
                let source = error
                    .source()
                    .unwrap()
                    .downcast_ref::<FixupError>()
                    .unwrap();
                assert!(matches!(
                    source,
                    FixupError::TrailerMismatch { stride: 1, .. }
                ));
                assert_eq!(error, RecordError::Fixup { source: *source });
                assert_eq!(
                    error.to_string(),
                    format!("FILE update-sequence fixup failed: {source}")
                );
                assert_eq!(bytes, before);
            } else {
                assert_eq!(parsed.unwrap().as_bytes()[510..512], [0, 0]);
            }
        }
    }
    for (offset, expected) in [
        (0x2bu16, RecordError::InvalidUsaOffset { offset: 0x2b }),
        (0x28, RecordError::InvalidUsaOffset { offset: 0x28 }),
    ] {
        let mut bytes = RecordBuilder::default().build();
        bytes[4..6].copy_from_slice(&offset.to_le_bytes());
        bytes[6..8].fill(0);
        let before = bytes.clone();
        assert_eq!(FileRecord::parse(&mut bytes).unwrap_err(), expected);
        assert_eq!(bytes, before);
    }
}

#[test]
fn parse_restores_trailers_and_preserves_classification() {
    for size in [512, 1024, 4096, 65536] {
        for usn in [0, 1, 0xffff, 0xa55a] {
            for base in [0, 7u64 << 48] {
                for flags in [0, 1] {
                    let builder = RecordBuilder {
                        size,
                        usn,
                        base,
                        flags,
                        ..Default::default()
                    };
                    let mut plain = builder.plain();
                    for stride in 0..size / 512 {
                        let tail = (stride + 1) * 512 - 2;
                        plain[tail..tail + 2].copy_from_slice(&(stride as u16 ^ usn).to_le_bytes());
                    }
                    let raw = builder.protect(&plain);
                    let mut parsed = raw.clone();
                    let view = FileRecord::parse(&mut parsed).unwrap();
                    assert_eq!(view.header().flags().raw(), flags);
                    for index in 0..size {
                        assert_eq!(
                            view.as_bytes()[index],
                            if index % 512 >= 510 {
                                plain[index]
                            } else {
                                raw[index]
                            }
                        );
                    }
                    let mut classified = raw.clone();
                    match RecordClass::classify(&mut classified) {
                        RecordClass::Base(record) if flags == 1 && base == 0 => {
                            assert_eq!(record.as_bytes(), view.as_bytes())
                        }
                        RecordClass::Extension(record) if flags == 1 && base != 0 => {
                            assert_eq!(record.as_bytes(), view.as_bytes())
                        }
                        RecordClass::NotInUse(header) if flags == 0 => {
                            assert_eq!(header, view.header())
                        }
                        other => panic!("unexpected class {other:?}"),
                    }
                    if flags == 0 {
                        assert_eq!(classified, raw);
                    }
                }
            }
        }
    }
}

proptest! {
    #![proptest_config(support::proptest_config("proptest-regressions/file_record.txt"))]
    #[test]
    fn peek_matches_fixed_header(flags in any::<u16>(), base in any::<u64>(), sequence in any::<u16>(), links in any::<u16>(), layout31 in any::<bool>()) {
        let mut bytes = RecordBuilder { flags, base, sequence, hard_links: links, usa_offset: if layout31 { 0x30 } else { 0x2a }, ..Default::default() }.build();
        let before = RecordHeader::peek(&bytes).unwrap();
        let record = FileRecord::parse(&mut bytes).unwrap();
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
        let actual = match RecordClass::classify(&mut bytes) {
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
        match RecordClass::classify(&mut bytes) {
            RecordClass::Base(record) => prop_assert_eq!(record.header(), header),
            other => prop_assert!(false, "unexpected class {other:?}"),
        }
    }

    #[test]
    fn arbitrary_record_operations_are_total_and_errors_are_atomic(input in prop::collection::vec(any::<u8>(), 0..=65536)) {
        let _ = RecordHeader::peek(&input);
        let mut parsed = input.clone();
        if FileRecord::parse(&mut parsed).is_err() { prop_assert_eq!(parsed, input.clone()); }
        let mut classified = input.clone();
        match RecordClass::classify(&mut classified) {
            RecordClass::Base(record) | RecordClass::Extension(record) => { let _ = record.header(); let _ = record.as_bytes(); },
            _ => prop_assert_eq!(classified, input),
        }
    }
}
