#![allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]
//! Bounded attribute iteration and non-resident stream descriptor contracts.

mod support;
use adamite_fixtures::builders::{AttrBody, AttrBuilder, NonResidentModel, RecordBuilder};
use adamite_ntfs::{
    attribute::{Attribute, AttributeError},
    record::FileRecord,
};
use proptest::prelude::*;

#[test]
fn basic_attribute_sequence_names_and_resident_values() {
    let mut named = AttrBuilder::resident(0x80, vec![1, 2, 3]);
    named.name = "meta".encode_utf16().collect();
    named.id = 7;
    named.flags = 0x4100;
    let mut bytes = RecordBuilder {
        attributes: vec![
            AttrBuilder::resident(0x10, vec![0; 72]).build(),
            AttrBuilder::resident(0x30, vec![0; 72]).build(),
            named.build(),
            AttrBuilder::resident(0x100, vec![]).build(),
        ],
        ..Default::default()
    }
    .build();
    let record = FileRecord::parse(&mut bytes).unwrap();
    let attrs: Vec<_> = record.attributes().collect::<Result<_, _>>().unwrap();
    assert_eq!(
        attrs
            .iter()
            .map(|attr| attr.attribute_type().raw())
            .collect::<Vec<_>>(),
        [0x10, 0x30, 0x80, 0x100]
    );
    assert_eq!(
        attrs[2].name().units().collect::<Vec<_>>(),
        "meta".encode_utf16().collect::<Vec<_>>()
    );
    assert_eq!(attrs[2].id(), 7);
    assert_eq!(attrs[2].flags().raw(), 0x4100);
    assert_eq!(attrs[2].resident_value(), Some(&[1, 2, 3][..]));
    assert_eq!(attrs[3].resident_value(), Some(&[][..]));
    assert!(attrs[2].is_resident());
}

#[test]
fn normal_records_and_unknown_attributes_between_known_types() {
    for unknown in [false, true] {
        let mut attributes = vec![
            AttrBuilder::resident(0x10, vec![0x5a; 72]).build(),
            AttrBuilder::resident(
                0x30,
                adamite_fixtures::builders::FileNameBuilder::default().build(),
            )
            .build(),
            AttrBuilder::resident(0x80, vec![]).build(),
        ];
        let mut expected = vec![0x10, 0x30, 0x80];
        if unknown {
            attributes.insert(1, AttrBuilder::resident(0x100, vec![]).build());
            expected.insert(1, 0x100);
        }
        let mut bytes = record(attributes);
        let attrs = decode(&mut bytes).unwrap();
        assert_eq!(
            attrs
                .iter()
                .map(|attribute| attribute.attribute_type().raw())
                .collect::<Vec<_>>(),
            expected
        );
        assert_eq!(attrs[0].resident_value(), Some(&[0x5a; 72][..]));
        assert_eq!(attrs.last().unwrap().resident_value(), Some(&[][..]));
    }
}

#[test]
fn compressed_and_sparse_extended_headers_report_compressed_size() {
    for flags in [1, 2, 0x8000] {
        let mut builder = AttrBuilder::non_resident(
            0x80,
            NonResidentModel {
                sizes: [8192, 6000, 5000, 4096],
                extended_header: true,
                ..Default::default()
            },
        );
        builder.flags = flags;
        let mut bytes = record(vec![builder.build()]);
        let attributes = decode(&mut bytes).unwrap();
        assert_eq!(attributes[0].flags().raw(), flags);
        assert_eq!(
            attributes[0]
                .non_resident()
                .unwrap()
                .sizes()
                .unwrap()
                .compressed(),
            Some(4096)
        );
    }
}

#[test]
fn zero_length_attribute_errors_once() {
    let mut attr = AttrBuilder::resident(0x80, vec![]).build();
    attr[4..8].fill(0);
    let mut bytes = RecordBuilder {
        attributes: vec![attr],
        ..Default::default()
    }
    .build();
    let record = FileRecord::parse(&mut bytes).unwrap();
    let mut iter = record.attributes();
    assert_eq!(
        iter.next(),
        Some(Err(AttributeError::InvalidLength { length: 0 }))
    );
    for _ in 0..16 {
        assert_eq!(iter.next(), None);
    }
}

#[test]
fn nonresident_descriptors_report_sizes_only_in_the_first_segment() {
    for lowest in [0, 300] {
        let model = NonResidentModel {
            lowest,
            highest: lowest,
            compression_unit: 4,
            sizes: if lowest == 0 {
                [1, 5000, 4000, 2]
            } else {
                [-1; 4]
            },
            mapping_pairs: vec![0x11, 1, 7, 0],
            extended_header: true,
        };
        let mut builder = AttrBuilder::non_resident(0x80, model);
        builder.flags = 0x8000;
        builder.body_gap = 1;
        let mut attr = builder.build();
        attr[0x23..0x28].fill(0xff);
        let mut bytes = RecordBuilder {
            attributes: vec![attr],
            ..Default::default()
        }
        .build();
        let record = FileRecord::parse(&mut bytes).unwrap();
        let attr = record.attributes().next().unwrap().unwrap();
        assert!(!attr.is_resident());
        assert!(attr.resident_value().is_none());
        let descriptor = attr.non_resident().unwrap();
        assert_eq!(descriptor.lowest_vcn(), lowest as u64);
        assert_eq!(descriptor.highest_vcn(), lowest);
        assert_eq!(descriptor.compression_unit(), 4);
        let runs: Vec<_> = Iterator::collect::<Result<Vec<_>, _>>(descriptor.runs()).unwrap();
        assert_eq!(runs[0].vcn(), lowest as u64);
        assert_eq!(runs[0].lcn(), Some(7));
        if lowest == 0 {
            let sizes = descriptor.sizes().unwrap();
            assert_eq!(
                (
                    sizes.allocated(),
                    sizes.data(),
                    sizes.initialized(),
                    sizes.compressed()
                ),
                (1, 5000, 4000, Some(2))
            );
        } else {
            assert!(descriptor.sizes().is_none());
        }
    }
}

fn decode(bytes: &mut [u8]) -> Result<Vec<Attribute<'_>>, AttributeError> {
    FileRecord::parse(bytes).unwrap().attributes().collect()
}

fn record(attributes: Vec<Vec<u8>>) -> Vec<u8> {
    RecordBuilder {
        size: 4096,
        attributes,
        ..Default::default()
    }
    .build()
}

fn error_once(attribute: Vec<u8>, expected: AttributeError) {
    let mut bytes = record(vec![attribute]);
    let record = FileRecord::parse(&mut bytes).unwrap();
    let mut iter = record.attributes();
    assert_eq!(iter.next(), Some(Err(expected)));
    for _ in 0..16 {
        assert_eq!(iter.next(), None);
    }
}

#[test]
fn malformed_headers_names_and_values_error_once() {
    let base = AttrBuilder::resident(0x80, vec![0; 72]).build();
    let mut attr = base.clone();
    attr[..4].fill(0);
    error_once(attr, AttributeError::InvalidType);
    let mut attr = base.clone();
    attr[8] = 2;
    error_once(attr, AttributeError::InvalidStorageForm { value: 2 });
    for length in [0, 8, 16, 23, 25, u32::MAX] {
        let mut attr = base.clone();
        attr[4..8].copy_from_slice(&length.to_le_bytes());
        error_once(attr, AttributeError::InvalidLength { length });
    }
    for offset in [0u16, 23, 95, u16::MAX] {
        let mut attr = base.clone();
        attr[9] = 4;
        attr[10..12].copy_from_slice(&offset.to_le_bytes());
        error_once(attr, AttributeError::InvalidNameRange);
    }
    let mut attr = base.clone();
    attr[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
    error_once(attr, AttributeError::InvalidValueRange);
    let mut attr = base;
    attr[20..22].copy_from_slice(&u16::MAX.to_le_bytes());
    error_once(attr, AttributeError::InvalidValueRange);
    let mut unaligned = AttrBuilder::resident(0x80, vec![1, 2, 3]);
    unaligned.name = vec![0xd800, 0, 0x61];
    unaligned.name_gap = 1;
    let mut bytes = record(vec![unaligned.build()]);
    let attrs = decode(&mut bytes).unwrap();
    assert_eq!(
        attrs[0].name().units().collect::<Vec<_>>(),
        [0xd800, 0, 0x61]
    );
    assert_eq!(attrs[0].resident_value(), Some(&[1, 2, 3][..]));
}

#[test]
fn used_bytes_end_marker_and_slack_bound_iteration() {
    let attr = AttrBuilder::resident(0x80, vec![7; 72]).build();
    let builder = RecordBuilder {
        size: 4096,
        attributes: vec![attr.clone()],
        ..Default::default()
    };
    let first = builder.first_attribute();
    let mut plain = builder.plain();
    plain[24..28].copy_from_slice(&((first + attr.len()) as u32).to_le_bytes());
    let mut bytes = builder.protect(&plain);
    assert_eq!(decode(&mut bytes), Err(AttributeError::MissingEndMarker));
    plain[24..28].copy_from_slice(&((first + attr.len() - 8) as u32).to_le_bytes());
    let mut bytes = builder.protect(&plain);
    assert_eq!(
        decode(&mut bytes),
        Err(AttributeError::AttributePastUsedBytes)
    );
    let mut plain = builder.plain();
    plain[first + attr.len() + 4..].fill(0xa5);
    let mut bytes = builder.protect(&plain);
    let parsed = decode(&mut bytes).unwrap();
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].resident_value(), Some(&[7; 72][..]));
}

#[test]
fn nonresident_header_and_numeric_failures_are_specific() {
    let base = AttrBuilder::non_resident(0x80, NonResidentModel::default()).build();
    for flags in [1u16, 2, 0x8000] {
        let mut attr = base[..64].to_vec();
        attr[4..8].copy_from_slice(&64u32.to_le_bytes());
        attr[12..14].copy_from_slice(&flags.to_le_bytes());
        error_once(attr, AttributeError::MissingExtendedHeader);
    }
    for offset in [0u16, 63, u16::MAX] {
        let mut attr = base.clone();
        attr[32..34].copy_from_slice(&offset.to_le_bytes());
        error_once(attr, AttributeError::InvalidMappingPairs);
    }
    let mut attr = base.clone();
    attr[16..24].copy_from_slice(&(-1i64).to_le_bytes());
    error_once(attr, AttributeError::InvalidVcnRange);
    for offset in [40, 48, 56, 64] {
        let mut builder = AttrBuilder::non_resident(0x80, NonResidentModel::default());
        builder.flags = 0x8000;
        let mut attr = builder.build();
        attr[offset..offset + 8].copy_from_slice(&(-1i64).to_le_bytes());
        error_once(attr, AttributeError::NegativeSize { offset });
    }
    let mut attr = base;
    attr[56..64].copy_from_slice(&5000i64.to_le_bytes());
    error_once(attr, AttributeError::InitializedPastData);
    let attr = AttrBuilder::non_resident(
        0x80,
        NonResidentModel {
            extended_header: true,
            sizes: [1, 5000, 4000, -1],
            ..Default::default()
        },
    )
    .build();
    let mut bytes = record(vec![attr]);
    assert_eq!(
        decode(&mut bytes).unwrap()[0]
            .non_resident()
            .unwrap()
            .sizes()
            .unwrap()
            .compressed(),
        None
    );
}

fn models() -> impl Strategy<Value = Vec<AttrBuilder>> {
    prop::collection::vec(
        (
            prop::sample::select(vec![0x10u32, 0x30, 0x80, 0x100, 0xa0]),
            any::<u16>(),
            any::<u16>(),
            prop::collection::vec(any::<u16>(), 0..=12),
            prop::collection::vec(any::<u8>(), 0..=80),
            any::<bool>(),
        )
            .prop_map(|(kind, id, flags, name, value, nonresident)| {
                let mut attr = if nonresident {
                    AttrBuilder::non_resident(kind, NonResidentModel::default())
                } else {
                    AttrBuilder::resident(kind, value)
                };
                attr.id = id;
                attr.flags = flags;
                attr.name = name;
                attr
            }),
        0..=8,
    )
}

proptest! {
    #![proptest_config(support::proptest_config("proptest-regressions/attributes.txt"))]
    #[test]
    fn sequences_round_trip_and_iterators_are_fused(models in models()) {
        let mut bytes = record(models.iter().map(AttrBuilder::build).collect());
        let record = FileRecord::parse(&mut bytes).unwrap();
        let mut iter = record.attributes();
        for model in &models {
            let attr = iter.next().unwrap().unwrap();
            prop_assert_eq!(attr.attribute_type().raw(), model.type_code); prop_assert_eq!(attr.id(), model.id); prop_assert_eq!(attr.flags().raw(), model.flags);
            prop_assert_eq!(attr.name().units().collect::<Vec<_>>(), model.name.clone());
            match &model.body {
                AttrBody::Resident(value) => prop_assert_eq!(attr.resident_value(), Some(value.as_slice())),
                AttrBody::NonResident(expected) => {
                    let descriptor = attr.non_resident().unwrap();
                    prop_assert!(descriptor.mapping_pairs().starts_with(&expected.mapping_pairs));
                    prop_assert_eq!(descriptor.lowest_vcn(), expected.lowest as u64);
                    prop_assert_eq!(descriptor.sizes().unwrap().data(), expected.sizes[1] as u64);
                }
            }
        }
        for _ in 0..16 { prop_assert_eq!(iter.next(), None); }
    }

    #[test]
    fn suffix_and_used_byte_boundaries_are_independent(models in models(), noise in prop::collection::vec(any::<u8>(), 4096)) {
        let builder = RecordBuilder { size: 4096, attributes: models.iter().map(AttrBuilder::build).collect(), ..Default::default() };
        let plain = builder.plain(); let first = builder.first_attribute();
        let used = u32::from_le_bytes(plain[24..28].try_into().unwrap()) as usize;
        let mut original = builder.protect(&plain); let expected = decode(&mut original).unwrap();
        let mut changed = plain.clone(); changed[used - 4..].copy_from_slice(&noise[used - 4..]);
        let mut bytes = builder.protect(&changed); prop_assert_eq!(decode(&mut bytes).unwrap(), expected);
        for end in ((first + 8)..used).step_by(8) {
            let mut changed = plain.clone(); changed[24..28].copy_from_slice(&(end as u32).to_le_bytes());
            let mut bytes = builder.protect(&changed);
            let record = FileRecord::parse(&mut bytes).unwrap();
            let mut iter = record.attributes(); let mut count = 0;
            for item in iter.by_ref() {
                count += 1; prop_assert!(count <= (end - first) / 24 + 1);
                if let Ok(attr) = item { prop_assert!(attr.as_bytes().len() <= end - first); }
            }
            for _ in 0..16 { prop_assert_eq!(iter.next(), None); }
        }
    }

    #[test]
    fn continuation_sizes_and_reserved_bytes_are_ignored(sizes in any::<[i64; 4]>(), reserved in any::<[u8; 5]>(), compression_unit in any::<u8>(), lowest in 1i64..1_000_000) {
        let mut builder = AttrBuilder::non_resident(0x80, NonResidentModel { lowest, highest: lowest, sizes, compression_unit, ..Default::default() }); builder.flags = 0x8000;
        let mut attr = builder.build(); attr[0x23..0x28].copy_from_slice(&reserved);
        let mut bytes = record(vec![attr]); let attrs = decode(&mut bytes).unwrap();
        let descriptor = attrs[0].non_resident().unwrap();
        prop_assert_eq!(descriptor.sizes(), None); prop_assert_eq!(descriptor.compression_unit(), compression_unit);
    }

    #[test]
    fn malformed_lengths_always_stop(length in any::<u32>()) {
        let mut attr = AttrBuilder::resident(0x80, vec![0; 72]).build(); attr[4..8].copy_from_slice(&length.to_le_bytes());
        let mut bytes = record(vec![attr]); let record = FileRecord::parse(&mut bytes).unwrap();
        let mut iter = record.attributes(); let mut count = 0;
        for _ in iter.by_ref() { count += 1; prop_assert!(count <= 4096 / 24 + 1); }
        for _ in 0..16 { prop_assert_eq!(iter.next(), None); }
    }
}
