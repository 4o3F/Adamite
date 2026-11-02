#![allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

//! File-reference encoding properties and well-known record scenarios.

use adamite_ntfs::{
    ATTR_DEF_RECORD, BAD_CLUS_RECORD, BITMAP_RECORD, BOOT_RECORD, EXTEND_RECORD,
    FIRST_NON_RESERVED_RECORD, FileRef, FileRefError, LOG_FILE_RECORD, MFT_MIRROR_RECORD,
    MFT_RECORD, ROOT_RECORD, SECURE_RECORD, UP_CASE_RECORD, VOLUME_RECORD,
};
use proptest::prelude::*;

fn boundary_u64() -> impl Strategy<Value = u64> {
    prop_oneof![
        4 => any::<u64>(),
        1 => prop::sample::select(vec![
            0, 1, (1 << 47) - 1, 1 << 47, (1 << 47) + 1,
            (1 << 48) - 1, 1 << 48, (1 << 48) + 1,
            (1 << 63) - 1, 1 << 63, (1 << 63) + 1, u64::MAX,
        ]),
    ]
}

proptest! {
    #[test]
    fn raw_and_bytes_round_trip(raw in boundary_u64()) {
        let reference = FileRef::from_raw(raw);
        let model = u128::from(raw);
        prop_assert_eq!(reference.raw(), raw);
        prop_assert_eq!(u128::from(reference.record()), model % (1u128 << 48));
        prop_assert_eq!(u128::from(reference.sequence()), model / (1u128 << 48));
        prop_assert_eq!(FileRef::from_le_bytes(raw.to_le_bytes()), reference);
        prop_assert_eq!(reference.to_le_bytes(), raw.to_le_bytes());
        prop_assert_eq!(FileRef::from_le_bytes(reference.to_le_bytes()), reference);
        prop_assert_eq!(reference.is_null(), raw == 0);
    }

    #[test]
    fn construction_accepts_exactly_48_bit_records(record in boundary_u64(), sequence in any::<u16>()) {
        let result = FileRef::new(record, sequence);
        if u128::from(record) < (1u128 << 48) {
            let reference = result.unwrap();
            let model = u128::from(sequence) * (1u128 << 48) + u128::from(record);
            prop_assert_eq!(u128::from(reference.raw()), model);
            prop_assert_eq!(reference.record(), record);
            prop_assert_eq!(reference.sequence(), sequence);
        } else {
            prop_assert_eq!(result, Err(FileRefError::RecordOutOfRange { record }));
        }
    }

    #[test]
    fn valid_parts_round_trip(record in 0u64..(1u64 << 48), sequence in any::<u16>()) {
        let reference = FileRef::new(record, sequence).unwrap();
        prop_assert_eq!(reference.record(), record);
        prop_assert_eq!(reference.sequence(), sequence);
    }
}

#[test]
fn construction_boundaries() {
    for sequence in [0, 1, u16::MAX] {
        let reference = FileRef::new((1 << 48) - 1, sequence).unwrap();
        assert_eq!(reference.record(), (1 << 48) - 1);
        assert_eq!(reference.sequence(), sequence);
        assert_eq!(
            FileRef::new(1 << 48, sequence),
            Err(FileRefError::RecordOutOfRange { record: 1 << 48 })
        );
    }
}

#[test]
fn only_the_all_zero_reference_is_null() {
    assert!(FileRef::from_raw(0).is_null());
    assert!(!FileRef::new(0, 3).unwrap().is_null());
    assert!(!FileRef::new(1, 0).unwrap().is_null());
    assert_eq!(format!("{:?}", FileRef::new(1234, 7).unwrap()), "1234:7");
}

#[test]
fn well_known_record_numbers() {
    assert_eq!(
        [
            MFT_RECORD,
            MFT_MIRROR_RECORD,
            LOG_FILE_RECORD,
            VOLUME_RECORD,
            ATTR_DEF_RECORD,
            ROOT_RECORD,
            BITMAP_RECORD,
            BOOT_RECORD,
            BAD_CLUS_RECORD,
            SECURE_RECORD,
            UP_CASE_RECORD,
            EXTEND_RECORD,
            FIRST_NON_RESERVED_RECORD,
        ],
        [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 16]
    );
}
