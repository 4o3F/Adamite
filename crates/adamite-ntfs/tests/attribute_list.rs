#![allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]
//! Complete attribute-list values, entry boundaries and independent encoders.

mod support;
use adamite_ntfs::{AttributeList, AttributeListError, FileRecord, fixup};
use proptest::prelude::*;
use support::builders::{
    AttrBuilder, AttrListBuilder, AttrListEntryModel, NonResidentModel, RecordBuilder,
};

fn default_value() -> Vec<u8> {
    AttrListBuilder {
        entries: vec![AttrListEntryModel::default()],
        zero_tail: 0,
    }
    .build()
}
fn first_error(input: &[u8], expected: AttributeListError) {
    let mut iter = AttributeList::new(input).unwrap();
    assert_eq!(iter.next(), Some(Err(expected)));
    for _ in 0..16 {
        assert_eq!(iter.next(), None);
    }
}

#[test]
fn mft_extension_reference_and_resident_attribute_adapter() {
    let value = default_value();
    let entry = AttributeList::new(&value).unwrap().next().unwrap().unwrap();
    assert_eq!(entry.attribute_type().raw(), 0x80);
    assert_eq!(entry.lowest_vcn(), 1000);
    assert_eq!(
        (
            entry.segment_reference().record(),
            entry.segment_reference().sequence()
        ),
        (17, 1)
    );
    assert_eq!(entry.attribute_id(), 9);
    assert_eq!(entry.name().len_units(), 0);
    let mut bytes = RecordBuilder {
        attributes: vec![AttrBuilder::resident(0x20, value).build()],
        ..Default::default()
    }
    .build();
    let record = FileRecord::parse(fixup::apply(&mut bytes).unwrap()).unwrap();
    assert_eq!(
        AttributeList::from_attribute(record.attributes().next().unwrap().unwrap())
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .lowest_vcn(),
        1000
    );
    let mut bytes = RecordBuilder {
        attributes: vec![AttrBuilder::resident(0x80, vec![]).build()],
        ..Default::default()
    }
    .build();
    let record = FileRecord::parse(fixup::apply(&mut bytes).unwrap()).unwrap();
    assert!(matches!(
        AttributeList::from_attribute(record.attributes().next().unwrap().unwrap()),
        Err(AttributeListError::Value {
            source: adamite_ntfs::ValueError::WrongAttributeType {
                expected: 0x20,
                actual: 0x80
            }
        })
    ));
    let mut bytes = RecordBuilder {
        attributes: vec![AttrBuilder::non_resident(0x20, NonResidentModel::default()).build()],
        ..Default::default()
    }
    .build();
    let record = FileRecord::parse(fixup::apply(&mut bytes).unwrap()).unwrap();
    assert!(matches!(
        AttributeList::from_attribute(record.attributes().next().unwrap().unwrap()),
        Err(AttributeListError::Value {
            source: adamite_ntfs::ValueError::WrongStorageForm
        })
    ));
}

#[test]
fn lengths_names_negative_vcns_and_short_zero_tails() {
    let base = default_value();
    let mut value = base.clone();
    value[4..6].copy_from_slice(&64u16.to_le_bytes());
    first_error(&value, AttributeListError::EntryPastValue);
    for length in 0u16..26 {
        let mut value = base.clone();
        value[4..6].copy_from_slice(&length.to_le_bytes());
        first_error(&value, AttributeListError::InvalidEntryLength { length });
    }
    let mut value = base.clone();
    value[8..16].copy_from_slice(&(-1i64).to_le_bytes());
    first_error(&value, AttributeListError::NegativeLowestVcn { value: -1 });
    for offset in [0u8, 25, 32, 255] {
        let mut value = base.clone();
        value[6] = 1;
        value[7] = offset;
        first_error(&value, AttributeListError::InvalidNameRange);
    }
    let mut odd = base.clone();
    odd[6] = 1;
    odd[7] = 27;
    odd[27..29].copy_from_slice(&0xd800u16.to_le_bytes());
    assert_eq!(
        AttributeList::new(&odd)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .name()
            .units()
            .collect::<Vec<_>>(),
        [0xd800]
    );
    let mut minimal = base[..26].to_vec();
    minimal[4..6].copy_from_slice(&26u16.to_le_bytes());
    assert_eq!(AttributeList::new(&minimal).unwrap().count(), 1);
    for tail in 0..26 {
        let value = vec![0; tail];
        let mut iter = AttributeList::new(&value).unwrap();
        for _ in 0..16 {
            assert_eq!(iter.next(), None);
        }
        if tail > 0 {
            first_error(&vec![1; tail], AttributeListError::TruncatedEntry);
        }
    }
    first_error(
        &[0; 26],
        AttributeListError::InvalidEntryLength { length: 0 },
    );
    assert!(AttributeList::new(&vec![0; 262144]).is_ok());
    assert!(matches!(
        AttributeList::new(&vec![0; 262145]),
        Err(AttributeListError::ValueTooLarge { len: 262145 })
    ));
}

fn models() -> impl Strategy<Value = Vec<AttrListEntryModel>> {
    prop::collection::vec(
        (
            any::<u32>(),
            0i64..=i64::MAX,
            any::<u64>(),
            any::<u16>(),
            prop::collection::vec(any::<u16>(), 0..=24),
        )
            .prop_map(
                |(type_code, lowest, segment, id, name)| AttrListEntryModel {
                    type_code,
                    lowest,
                    segment,
                    id,
                    name,
                },
            ),
        0..=8,
    )
}

proptest! {
    #![proptest_config(support::proptest_config("proptest-regressions/attribute_list.txt"))]
    #[test]
    fn entries_round_trip_and_iteration_is_bounded_and_fused(models in models(), tail in 0usize..26) {
        let value = AttrListBuilder { entries: models.clone(), zero_tail: tail }.build(); let mut iter = AttributeList::new(&value).unwrap();
        for model in &models {
            let entry = iter.next().unwrap().unwrap(); prop_assert_eq!(entry.attribute_type().raw(), model.type_code); prop_assert_eq!(entry.lowest_vcn(), model.lowest as u64);
            prop_assert_eq!(entry.segment_reference().raw(), model.segment); prop_assert_eq!(entry.attribute_id(), model.id); prop_assert_eq!(entry.name().units().collect::<Vec<_>>(), model.name.clone());
        }
        prop_assert!(models.len() <= value.len() / 26);
        for _ in 0..16 { prop_assert_eq!(iter.next(), None); }
    }

    #[test]
    fn every_truncation_obeys_entry_boundaries_and_short_zero_tail_rule(models in models()) {
        let value = AttrListBuilder { entries: models.clone(), zero_tail: 0 }.build();
        let mut boundaries = vec![0usize]; let mut end = 0u128;
        for model in &models { end += (26u128 + model.name.len() as u128 * 2).div_ceil(8) * 8; boundaries.push(end as usize); }
        for cut in 0..=value.len() {
            let boundary = *boundaries.iter().rfind(|&&position| position <= cut).unwrap();
            let expected = boundary == cut || (cut - boundary < 26 && value[boundary..cut].iter().all(|&byte| byte == 0));
            let mut iter = AttributeList::new(&value[..cut]).unwrap(); let mut count = 0; let mut error = false;
            for entry in iter.by_ref() { count += 1; error |= entry.is_err(); prop_assert!(count <= cut / 26 + 1); }
            prop_assert_eq!(!error, expected, "cut={}, boundary={}", cut, boundary);
            for _ in 0..16 { prop_assert_eq!(iter.next(), None); }
        }
    }
}
