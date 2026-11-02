#![allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]
//! Typed payload scenarios and independent semantic round-trip properties.

mod support;
use adamite_ntfs::{
    Attribute, AttributeList, Bitmap, BitmapError, Data, FileAttributes, FileName, FileRecord,
    IndexAllocation, ReparsePoint, StandardInformation, ValueError, fixup,
};
use proptest::prelude::*;
use support::builders::{AttrBuilder, FileNameBuilder, NonResidentModel, RecordBuilder};

fn record(attributes: Vec<Vec<u8>>) -> Vec<u8> {
    RecordBuilder {
        size: 4096,
        attributes,
        ..Default::default()
    }
    .build()
}

#[test]
fn cached_file_name_size_and_data_size_remain_independent() {
    let name = FileNameBuilder {
        data: 0,
        allocated: 0,
        ..Default::default()
    }
    .build();
    let mut bytes = RecordBuilder {
        size: 16384,
        attributes: vec![
            AttrBuilder::resident(0x30, name).build(),
            AttrBuilder::resident(0x80, vec![0; 5000]).build(),
        ],
        ..Default::default()
    }
    .build();
    let record = FileRecord::parse(fixup::apply(&mut bytes).unwrap()).unwrap();
    let mut attrs = record.attributes();
    let file_name = FileName::from_attribute(attrs.next().unwrap().unwrap()).unwrap();
    let data = Data::from_attribute(attrs.next().unwrap().unwrap()).unwrap();
    assert_eq!(file_name.cached_data_size(), 0);
    assert_eq!(data.logical_size(), Some(5000));
    assert_eq!(file_name.cached_allocated_size(), 0);
}

fn exhaust_name(name: adamite_ntfs::Utf16Le<'_>) {
    assert_eq!(name.units().count(), name.len_units());
    let _ = name.decode_strict().count();
    let _ = name.decode_lossy().count();
    #[cfg(feature = "alloc")]
    {
        let _ = name.to_string_strict();
        let _ = name.to_string_lossy();
    }
}

fn exercise_attribute(attribute: Attribute<'_>) {
    let _ = StandardInformation::from_attribute(attribute);
    if let Ok(value) = FileName::from_attribute(attribute) {
        exhaust_name(value.name());
    }
    if let Ok(value) = Data::from_attribute(attribute) {
        exhaust_name(value.name());
        let _ = value.logical_size();
        if let Some(descriptor) = value.non_resident() {
            let bound = descriptor.mapping_pairs().len() / 2 + 1;
            for (index, _) in descriptor.runs().enumerate() {
                assert!(index < bound);
            }
        }
    }
    if let Ok(value) = IndexAllocation::from_attribute(attribute) {
        exhaust_name(value.name());
        let descriptor = value.descriptor();
        let bound = descriptor.mapping_pairs().len() / 2 + 1;
        for (index, _) in descriptor.runs().enumerate() {
            assert!(index < bound);
        }
    }
    let _ = ReparsePoint::from_attribute(attribute);
    if let Ok(iter) = AttributeList::from_attribute(attribute) {
        for (index, entry) in iter.enumerate() {
            assert!(index <= attribute.as_bytes().len() / 26);
            if let Ok(entry) = entry {
                exhaust_name(entry.name());
            }
        }
    }
}

fn arbitrary_value() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        4 => prop::collection::vec(any::<u8>(), 0..=65536),
        1 => prop::sample::select(vec![0usize, 1, 7, 8, 47, 48, 49, 65, 66, 67, 71, 72, 16384, 16385, 65536]).prop_flat_map(|length| prop::collection::vec(any::<u8>(), length)),
    ]
}

proptest! {
    #![proptest_config(support::proptest_config("proptest-regressions/typed_values.txt"))]
    #[test]
    fn every_typed_decoder_is_total(input in arbitrary_value(), logical_bits in any::<usize>()) {
        let _ = StandardInformation::parse(&input);
        if let Ok(value) = FileName::parse(&input) { exhaust_name(value.name()); }
        let _ = ReparsePoint::parse(&input);
        let mut entries = AttributeList::new(&input).unwrap();
        for (index, entry) in entries.by_ref().enumerate() {
            prop_assert!(index <= input.len() / 26);
            if let Ok(entry) = entry { exhaust_name(entry.name()); }
        }
        for _ in 0..16 { prop_assert_eq!(entries.next(), None); }
        if let Ok(bitmap) = Bitmap::new(&input, logical_bits) { let _ = bitmap.iter().count(); let _ = bitmap.iter_set().count(); }
        let bitmap = Bitmap::new(&input, input.len().min(1024) * 8).unwrap(); let _ = bitmap.iter().count(); let _ = bitmap.iter_set().count();
        // Chunks fit the maximum FILE record size while covering every input byte.
        for start in (0..=input.len()).step_by(60000) {
            let payload = &input[start..input.len().min(start + 60000)];
            for kind in [0x10, 0x20, 0x30, 0x80, 0xa0, 0xb0, 0xc0] {
                for nonresident in [false, true] {
                    let builder = if nonresident { AttrBuilder::non_resident(kind, NonResidentModel { mapping_pairs: payload.to_vec(), ..Default::default() }) } else { AttrBuilder::resident(kind, payload.to_vec()) };
                    let mut bytes = RecordBuilder { size: 65536, attributes: vec![builder.build()], ..Default::default() }.build();
                    exercise_attribute(first(&mut bytes));
                }
            }
        }
    }
}

fn reparse_value(tag: u32, declared: u16, payload: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend(tag.to_le_bytes());
    bytes.extend(declared.to_le_bytes());
    bytes.extend([0x12, 0x34]);
    bytes.extend(payload);
    bytes
}

#[test]
fn reparse_values_preserve_tags_and_all_payload_bytes() {
    for tag in [0xa000000c, 0, u32::MAX] {
        let value = reparse_value(tag, 2, &[1, 2, 3, 4]);
        let parsed = ReparsePoint::parse(&value).unwrap();
        assert_eq!(parsed.tag(), tag);
        assert_eq!(parsed.data_length(), 2);
        assert_eq!(parsed.payload(), &[1, 2, 3, 4]);
        let mut bytes = record(vec![AttrBuilder::resident(0xc0, value).build()]);
        let parsed = ReparsePoint::from_attribute(first(&mut bytes)).unwrap();
        assert_eq!(parsed.payload(), &[1, 2, 3, 4]);
    }
    assert_eq!(
        ReparsePoint::parse(&reparse_value(0, 5, &[0; 4])),
        Err(ValueError::InvalidReparseLength)
    );
    assert_eq!(
        ReparsePoint::parse(&vec![0; 16385]),
        Err(ValueError::ReparseValueTooLarge { len: 16385 })
    );
    for len in 0..8 {
        assert_eq!(
            ReparsePoint::parse(&vec![0; len]),
            Err(ValueError::TruncatedValue)
        );
    }
    let mut bytes = record(vec![AttrBuilder::resident(0x80, vec![]).build()]);
    assert_eq!(
        ReparsePoint::from_attribute(first(&mut bytes)),
        Err(ValueError::WrongAttributeType {
            expected: 0xc0,
            actual: 0x80
        })
    );
    let mut bytes = record(vec![
        AttrBuilder::non_resident(0xc0, NonResidentModel::default()).build(),
    ]);
    assert_eq!(
        ReparsePoint::from_attribute(first(&mut bytes)),
        Err(ValueError::WrongStorageForm)
    );
}

#[test]
fn bitmap_scenarios_and_fused_iteration() {
    let bitmap = Bitmap::new(&[0b0000_0101], 8).unwrap();
    assert_eq!(bitmap.logical_bits(), 8);
    assert_eq!(bitmap.iter_set().collect::<Vec<_>>(), [0, 2]);
    assert_eq!(bitmap.get(8), None);
    assert_eq!(bitmap.get(usize::MAX), None);
    let bitmap = Bitmap::new(&[0b1111_1101], 3).unwrap();
    assert_eq!(bitmap.iter().collect::<Vec<_>>(), [true, false, true]);
    assert_eq!(bitmap.get(3), None);
    let mut bits = bitmap.iter();
    assert_eq!(bits.len(), 3);
    assert_eq!(bits.next(), Some(true));
    assert_eq!(bits.len(), 2);
    assert_eq!(bits.by_ref().count(), 2);
    let mut set = bitmap.iter_set();
    assert_eq!(set.by_ref().collect::<Vec<_>>(), [0, 2]);
    for _ in 0..16 {
        assert_eq!(bits.next(), None);
        assert_eq!(set.next(), None);
    }
    assert_eq!(
        Bitmap::new(&[0], 9),
        Err(BitmapError::LogicalBitsOutOfRange {
            logical_bits: 9,
            byte_len: 1
        })
    );
    assert!(Bitmap::new(&[], 0).unwrap().iter().next().is_none());
}

proptest! {
    #![proptest_config(support::proptest_config("proptest-regressions/typed_values.txt"))]
    #[test]
    fn reparse_length_rules_and_tag_payload_round_trip(tag in any::<u32>(), declared in any::<u16>(), length in prop_oneof![0usize..=16400, prop::sample::select(vec![0usize, 7, 8, 9, 16383, 16384, 16385])], fill in any::<u8>()) {
        let mut value = vec![fill; length];
        if length >= 8 { value[..4].copy_from_slice(&tag.to_le_bytes()); value[4..6].copy_from_slice(&declared.to_le_bytes()); }
        let result = ReparsePoint::parse(&value);
        prop_assert_eq!(result.is_ok(), (8..=16384).contains(&length) && u128::from(declared) + 8 <= length as u128);
        if let Ok(parsed) = result { prop_assert_eq!(parsed.tag(), tag); prop_assert_eq!(parsed.data_length(), declared); prop_assert_eq!(parsed.payload(), &value[8..]); }
    }

    #[test]
    fn bitmap_matches_naive_model(input in prop::collection::vec(any::<u8>(), 0..=512), selection in any::<usize>()) {
        let capacity = input.len() * 8; let logical = selection % (capacity + 1);
        let model: Vec<_> = input.iter().flat_map(|byte| (0..8).map(move |bit| byte & (1 << bit) != 0)).take(logical).collect();
        let bitmap = Bitmap::new(&input, logical).unwrap();
        prop_assert_eq!(bitmap.iter().collect::<Vec<_>>(), model.clone()); prop_assert_eq!(bitmap.iter().count(), logical);
        for (index, expected) in model.iter().enumerate() { prop_assert_eq!(bitmap.get(index), Some(*expected)); }
        let set: Vec<_> = bitmap.iter_set().collect();
        prop_assert_eq!(&set, &model.iter().enumerate().filter_map(|(index, &bit)| bit.then_some(index)).collect::<Vec<_>>());
        prop_assert!(set.windows(2).all(|pair| pair[0] < pair[1]));
        prop_assert_eq!(bitmap.get(logical), None); prop_assert_eq!(bitmap.get(usize::MAX), None);
        prop_assert!(Bitmap::new(&input, capacity + 1).is_err());
    }
}

#[test]
fn data_streams_are_reported_separately_with_resident_lengths() {
    let unnamed = AttrBuilder::resident(0x80, vec![1; 5]);
    let mut named = AttrBuilder::resident(0x80, vec![2; 4]);
    named.name = "meta".encode_utf16().collect();
    named.flags = 0x4100;
    let mut bytes = record(vec![unnamed.build(), named.build()]);
    let record = FileRecord::parse(fixup::apply(&mut bytes).unwrap()).unwrap();
    let streams: Vec<_> = record
        .attributes()
        .map(|attr| Data::from_attribute(attr.unwrap()).unwrap())
        .collect();
    assert_eq!(streams.len(), 2);
    assert_eq!(streams[0].name().len_units(), 0);
    assert_eq!(streams[0].logical_size(), Some(5));
    assert_eq!(streams[1].name().decode_lossy().collect::<String>(), "meta");
    assert_eq!(streams[1].logical_size(), Some(4));
    assert_eq!(streams[1].flags().raw(), 0x4100);
    assert_eq!(streams[1].resident_value(), Some(&[2; 4][..]));
    assert!(streams[1].non_resident().is_none());
}

#[test]
fn nonresident_data_and_directory_index_preserve_segment_descriptors() {
    for lowest in [0, 300] {
        let model = NonResidentModel {
            lowest,
            highest: lowest,
            sizes: if lowest == 0 {
                [1, 5000, 4000, 2]
            } else {
                [-1; 4]
            },
            ..Default::default()
        };
        let mut data = AttrBuilder::non_resident(0x80, model.clone());
        data.flags = 0x8000;
        let mut index = AttrBuilder::non_resident(0xa0, model);
        index.name = "$I30".encode_utf16().collect();
        index.flags = 0x4000;
        let mut bytes = record(vec![data.build(), index.build()]);
        let record = FileRecord::parse(fixup::apply(&mut bytes).unwrap()).unwrap();
        let mut attrs = record.attributes();
        let data = Data::from_attribute(attrs.next().unwrap().unwrap()).unwrap();
        assert!(data.resident_value().is_none());
        assert_eq!(data.name().len_units(), 0);
        assert_eq!(data.flags().raw(), 0x8000);
        assert_eq!(data.logical_size(), (lowest == 0).then_some(5000));
        let descriptor = data.non_resident().unwrap();
        assert_eq!(descriptor.lowest_vcn(), lowest as u64);
        assert!(descriptor.mapping_pairs().starts_with(&[0x11, 1, 1, 0]));
        if lowest == 0 {
            assert_eq!(descriptor.sizes().unwrap().initialized(), 4000);
        }
        let index = IndexAllocation::from_attribute(attrs.next().unwrap().unwrap()).unwrap();
        assert_eq!(index.name().decode_lossy().collect::<String>(), "$I30");
        assert_eq!(index.flags().raw(), 0x4000);
        assert_eq!(
            index.descriptor().sizes().map(|sizes| sizes.allocated()),
            (lowest == 0).then_some(1)
        );
    }
    let mut bytes = record(vec![AttrBuilder::resident(0x30, vec![]).build()]);
    let attr = first(&mut bytes);
    assert_eq!(
        Data::from_attribute(attr),
        Err(ValueError::WrongAttributeType {
            expected: 0x80,
            actual: 0x30
        })
    );
    assert_eq!(
        IndexAllocation::from_attribute(attr),
        Err(ValueError::WrongAttributeType {
            expected: 0xa0,
            actual: 0x30
        })
    );
    let mut bytes = record(vec![AttrBuilder::resident(0xa0, vec![]).build()]);
    assert_eq!(
        IndexAllocation::from_attribute(first(&mut bytes)),
        Err(ValueError::WrongStorageForm)
    );
}
fn first(bytes: &mut [u8]) -> Attribute<'_> {
    FileRecord::parse(fixup::apply(bytes).unwrap())
        .unwrap()
        .attributes()
        .next()
        .unwrap()
        .unwrap()
}
fn standard_value(times: [u64; 4], flags: u32, extra: Option<(u32, u32, u64, u64)>) -> Vec<u8> {
    let mut bytes = vec![0; if extra.is_some() { 72 } else { 48 }];
    for (index, time) in times.iter().enumerate() {
        bytes[index * 8..index * 8 + 8].copy_from_slice(&time.to_le_bytes());
    }
    bytes[32..36].copy_from_slice(&flags.to_le_bytes());
    if let Some((owner, security, quota, usn)) = extra {
        bytes[48..52].copy_from_slice(&owner.to_le_bytes());
        bytes[52..56].copy_from_slice(&security.to_le_bytes());
        bytes[56..64].copy_from_slice(&quota.to_le_bytes());
        bytes[64..72].copy_from_slice(&usn.to_le_bytes());
    }
    bytes
}

#[test]
fn standard_information_legacy_extended_and_lenient_tail() {
    let times = [11, 22, 33, 44];
    for extended in [false, true] {
        let value = standard_value(times, 0x8000_0006, extended.then_some((7, 8, 9, 10)));
        let parsed = StandardInformation::parse(&value).unwrap();
        assert_eq!(
            (
                parsed.times().created(),
                parsed.times().modified(),
                parsed.times().record_changed(),
                parsed.times().accessed()
            ),
            (11, 22, 33, 44)
        );
        assert!(parsed.file_attributes().contains(FileAttributes::HIDDEN));
        assert!(parsed.file_attributes().contains(FileAttributes::SYSTEM));
        assert_eq!(parsed.file_attributes().raw(), 0x8000_0006);
        assert_eq!(
            (
                parsed.owner_id(),
                parsed.security_id(),
                parsed.quota_charged(),
                parsed.usn()
            ),
            if extended {
                (Some(7), Some(8), Some(9), Some(10))
            } else {
                (None, None, None, None)
            }
        );
        let mut bytes = record(vec![AttrBuilder::resident(0x10, value).build()]);
        assert_eq!(
            StandardInformation::from_attribute(first(&mut bytes)).unwrap(),
            parsed
        );
    }
    let mut value = standard_value(times, 0, Some((1, 2, 3, 4)));
    let original = StandardInformation::parse(&value).unwrap();
    value.extend([0xff; 8]);
    assert_eq!(StandardInformation::parse(&value), Ok(original));
    for len in 0..=100 {
        assert_eq!(
            StandardInformation::parse(&vec![0; len]).is_ok(),
            len == 48 || len >= 72
        );
    }
}

#[test]
fn file_names_preserve_namespace_metadata_and_raw_units() {
    let model = FileNameBuilder::default();
    let value = model.build();
    let parsed = FileName::parse(&value).unwrap();
    assert_eq!(
        (parsed.parent().record(), parsed.parent().sequence()),
        (5, 5)
    );
    assert_eq!(parsed.namespace(), 1);
    assert_eq!(
        parsed.name().decode_lossy().collect::<String>(),
        "notes.txt"
    );
    assert_eq!(
        (parsed.cached_allocated_size(), parsed.cached_data_size()),
        (4096, 123)
    );
    assert_eq!(parsed.ea_size_or_reparse_tag(), 0);
    let mut bytes = record(vec![AttrBuilder::resident(0x30, value.clone()).build()]);
    assert_eq!(FileName::from_attribute(first(&mut bytes)).unwrap(), parsed);
    let mut trailing = value.clone();
    trailing.extend([0xff; 13]);
    assert_eq!(FileName::parse(&trailing).unwrap(), parsed);
    for len in 0..value.len() {
        assert!(FileName::parse(&value[..len]).is_err());
    }
    let mut invalid = value.clone();
    invalid[65] = 4;
    assert_eq!(
        FileName::parse(&invalid),
        Err(ValueError::InvalidNamespace { namespace: 4 })
    );
    let mut invalid = value;
    invalid[64] = 0;
    assert_eq!(
        FileName::parse(&invalid),
        Err(ValueError::InvalidNameLength)
    );
    let value = FileNameBuilder {
        name: vec![0xd83d, 0xde00, 0, 0xd800, 0x61],
        ..Default::default()
    }
    .build();
    let name = FileName::parse(&value).unwrap().name();
    assert_eq!(
        name.units().collect::<Vec<_>>(),
        [0xd83d, 0xde00, 0, 0xd800, 0x61]
    );
    assert_eq!(name.decode_lossy().collect::<String>(), "😀\0\u{fffd}a");
    assert!(name.decode_strict().collect::<Result<String, _>>().is_err());
    #[cfg(feature = "alloc")]
    assert_eq!(name.to_string_lossy().unwrap(), "😀\0\u{fffd}a");
}

#[test]
fn typed_metadata_decoders_reject_wrong_type_and_storage() {
    let mut bytes = record(vec![AttrBuilder::resident(0x80, vec![]).build()]);
    let attr = first(&mut bytes);
    assert_eq!(
        FileName::from_attribute(attr),
        Err(ValueError::WrongAttributeType {
            expected: 0x30,
            actual: 0x80
        })
    );
    assert_eq!(
        StandardInformation::from_attribute(attr),
        Err(ValueError::WrongAttributeType {
            expected: 0x10,
            actual: 0x80
        })
    );
    for kind in [0x10, 0x30] {
        let mut bytes = record(vec![
            AttrBuilder::non_resident(kind, NonResidentModel::default()).build(),
        ]);
        let attr = first(&mut bytes);
        if kind == 0x10 {
            assert_eq!(
                StandardInformation::from_attribute(attr),
                Err(ValueError::WrongStorageForm)
            );
        } else {
            assert_eq!(
                FileName::from_attribute(attr),
                Err(ValueError::WrongStorageForm)
            );
        }
    }
}

proptest! {
    #![proptest_config(support::proptest_config("proptest-regressions/typed_values.txt"))]
    #[test]
    fn standard_information_round_trip(times in any::<[u64; 4]>(), flags in any::<u32>(), owner in any::<u32>(), security in any::<u32>(), quota in any::<u64>(), usn in any::<u64>(), extended in any::<bool>(), length in 0usize..=100) {
        let value = standard_value(times, flags, extended.then_some((owner, security, quota, usn)));
        let parsed = StandardInformation::parse(&value).unwrap();
        prop_assert_eq!([parsed.times().created(), parsed.times().modified(), parsed.times().record_changed(), parsed.times().accessed()], times);
        prop_assert_eq!(parsed.file_attributes().raw(), flags);
        prop_assert_eq!((parsed.owner_id(), parsed.security_id(), parsed.quota_charged(), parsed.usn()), if extended { (Some(owner), Some(security), Some(quota), Some(usn)) } else { (None, None, None, None) });
        prop_assert_eq!(StandardInformation::parse(&vec![0; length]).is_ok(), length == 48 || length >= 72);
    }

    #[test]
    fn file_name_round_trip_and_length_namespace_rules(parent in any::<u64>(), times in any::<[u64; 4]>(), allocated in any::<u64>(), data in any::<u64>(), flags in any::<u32>(), word in any::<u32>(), namespace in 0u8..=5, units in prop::collection::vec(any::<u16>(), 0..=256), suffix in prop::collection::vec(any::<u8>(), 0..=16)) {
        let model = FileNameBuilder { parent, times, allocated, data, attributes: flags, ea_or_reparse: word, namespace, name: units.clone() };
        let mut value = model.build(); value.extend(suffix);
        let parsed = FileName::parse(&value);
        prop_assert_eq!(parsed.is_ok(), (1..=255).contains(&units.len()) && namespace <= 3);
        if let Ok(parsed) = parsed {
            prop_assert_eq!(parsed.parent().raw(), parent); prop_assert_eq!([parsed.times().created(), parsed.times().modified(), parsed.times().record_changed(), parsed.times().accessed()], times);
            prop_assert_eq!((parsed.cached_allocated_size(), parsed.cached_data_size()), (allocated, data));
            prop_assert_eq!(parsed.file_attributes().raw(), flags); prop_assert_eq!(parsed.ea_size_or_reparse_tag(), word);
            prop_assert_eq!(parsed.namespace(), namespace); prop_assert_eq!(parsed.name().units().collect::<Vec<_>>(), units);
        }
    }
}
