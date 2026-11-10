#![allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

//! Mapping-pair scenarios, independent encoders and wide-integer models.

mod support;

use adamite_fixtures::builders::BootSectorBuilder;
use adamite_fixtures::runlist_encoder::{RunModel, RunlistEncoder};
use adamite_ntfs::{Run, Runlist, RunlistError, VcnRange, parse_boot_sector};
use proptest::prelude::*;

fn decode(bytes: &[u8], lowest: i64, highest: i64) -> Result<Vec<Run>, RunlistError> {
    Runlist::new(bytes, VcnRange::new(lowest, highest)?).collect()
}

#[test]
fn documented_single_run_and_empty_stream() {
    // https://flatcap.github.io/linux-ntfs/ntfs/concepts/data_runs.html, example 1.
    let runs = decode(&[0x21, 0x18, 0x34, 0x56, 0], 0, 23).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(
        (runs[0].vcn(), runs[0].length(), runs[0].lcn()),
        (0, 0x18, Some(0x5634))
    );
    assert!(decode(&[0], 0, -1).unwrap().is_empty());
}

fn raw_pair(length: u64, delta: Option<i64>) -> Vec<u8> {
    let mut bytes = vec![if delta.is_some() { 0x88 } else { 0x08 }];
    bytes.extend(length.to_le_bytes());
    if let Some(delta) = delta {
        bytes.extend(delta.to_le_bytes());
    }
    bytes
}

fn assert_error(bytes: &[u8], lowest: i64, highest: i64, expected: RunlistError) {
    let mut runs = Runlist::new(bytes, VcnRange::new(lowest, highest).unwrap());
    let items: Vec<_> = runs.by_ref().collect();
    assert_eq!(items.last(), Some(&Err(expected)), "bytes={bytes:02x?}");
    assert!(items[..items.len() - 1].iter().all(Result::is_ok));
    assert!(items.len() <= bytes.len() / 2 + 1);
    for _ in 0..16 {
        assert_eq!(runs.next(), None);
    }
}

#[test]
fn documented_negative_delta_and_other_golden_runs() {
    // https://flatcap.github.io/linux-ntfs/ntfs/concepts/data_runs.html, example 3.
    let runs = decode(
        &[
            0x11, 0x30, 0x60, 0x21, 0x10, 0x00, 0x01, 0x11, 0x20, 0xe0, 0,
        ],
        0,
        0x5f,
    )
    .unwrap();
    assert_eq!(
        runs.iter()
            .map(|run| (run.vcn(), run.length(), run.lcn()))
            .collect::<Vec<_>>(),
        [
            (0, 0x30, Some(0x60)),
            (0x30, 0x10, Some(0x160)),
            (0x40, 0x20, Some(0x140)),
        ]
    );
    let runs = decode(&[0x21, 1, 0xe8, 3, 0x11, 1, 0xf6, 0], 0, 1).unwrap();
    assert_eq!((runs[0].lcn(), runs[1].lcn()), (Some(1000), Some(990)));
    assert_eq!(
        decode(&[0x11, 0x80, 1, 0], 0, 127).unwrap()[0].length(),
        128
    );
    let mut eight = raw_pair(128, Some(0x5634));
    eight.push(0);
    assert_eq!(decode(&eight, 0, 127).unwrap()[0].lcn(), Some(0x5634));
}

#[test]
fn sparse_base_zero_lcn_and_extension_segment() {
    let runs = decode(&[0x11, 1, 100, 0x01, 16, 0x11, 1, 50, 0], 0, 17).unwrap();
    assert_eq!(
        runs.iter()
            .map(|run| (run.vcn(), run.lcn()))
            .collect::<Vec<_>>(),
        [(0, Some(100)), (1, None), (17, Some(150))]
    );
    let leading = decode(&[0x01, 1, 0x21, 1, 0xd0, 7, 0], 0, 1).unwrap();
    assert_eq!((leading[0].lcn(), leading[1].lcn()), (None, Some(2000)));
    assert_eq!(decode(&[0x11, 1, 0, 0], 0, 0).unwrap()[0].lcn(), Some(0));
    let extension = decode(&[0x21, 1, 0xd0, 7, 0], 500, 500).unwrap();
    assert_eq!((extension[0].vcn(), extension[0].lcn()), (500, Some(2000)));
}

#[test]
fn range_rules_include_literal_zero_and_empty_stream() {
    for lcn in [None, Some(0)] {
        let mut maximum_length = raw_pair(i64::MAX as u64, lcn);
        maximum_length.push(0);
        let run = decode(&maximum_length, 0, i64::MAX - 1).unwrap()[0];
        assert_eq!(run.length(), i64::MAX as u64);
    }
    let empty = VcnRange::new(0, -1).unwrap();
    assert!(empty.is_empty());
    assert_eq!(empty.lowest_vcn(), 0);
    assert_eq!(empty.highest_vcn(), -1);
    let one = VcnRange::new(0, 0).unwrap();
    assert!(!one.is_empty());
    assert_eq!(one.highest_vcn(), 0);
    assert_eq!(decode(&[0x01, 1, 0], 0, 0).unwrap().len(), 1);
    assert_error(
        &[0],
        0,
        0,
        RunlistError::CoverageMismatch {
            expected: 1,
            actual: 0,
        },
    );
    for (lowest, highest) in [
        (1, -1),
        (2, 0),
        (1, 0),
        (-1, -1),
        (-1, 0),
        (0, -2),
        (0, i64::MAX),
        (i64::MAX, i64::MAX - 1),
    ] {
        assert_eq!(
            VcnRange::new(lowest, highest),
            Err(RunlistError::InvalidVcnRange { lowest, highest })
        );
    }
    let mut maximal = raw_pair(1, Some(i64::MAX - 1));
    maximal.push(0);
    let run = decode(&maximal, i64::MAX - 1, i64::MAX - 1).unwrap()[0];
    assert_eq!(run.vcn(), i64::MAX as u64 - 1);
    assert_eq!(run.lcn(), Some(i64::MAX as u64 - 1));
}

#[test]
fn every_width_header_and_truncated_field_is_checked() {
    for header in 1..=255u8 {
        let length = header & 15;
        let offset = header >> 4;
        let expected = if !(1..=8).contains(&length) || offset > 8 {
            RunlistError::InvalidFieldWidths { length, offset }
        } else {
            RunlistError::TruncatedPair
        };
        assert_error(&[header], 0, 0, expected);
    }
    let mut encoded = raw_pair(1, Some(10));
    encoded.push(0);
    assert_error(&[], 0, 0, RunlistError::MissingTerminator);
    for cut in 1..17 {
        assert_error(&encoded[..cut], 0, 0, RunlistError::TruncatedPair);
    }
    assert_error(&encoded[..17], 0, 0, RunlistError::MissingTerminator);
    assert!(decode(&encoded, 0, 0).is_ok());
}

#[test]
fn coverage_zero_lengths_overflow_and_negative_lcns() {
    assert_error(
        &[0x11, 10, 1, 0],
        0,
        10,
        RunlistError::CoverageMismatch {
            expected: 11,
            actual: 10,
        },
    );
    assert_error(
        &[0x11, 10, 1, 0],
        0,
        8,
        RunlistError::CoverageMismatch {
            expected: 9,
            actual: 10,
        },
    );
    assert_error(&[0x11, 0, 0, 0], 0, 0, RunlistError::ZeroRunLength);
    assert_error(
        &[0x11, 1, 5, 0x11, 1, 0xfa, 0],
        0,
        1,
        RunlistError::LcnOutOfRange { lcn: -1 },
    );
    for length in [1u64 << 63, u64::MAX] {
        assert_error(
            &raw_pair(length, None),
            0,
            0,
            RunlistError::RunLengthOutOfRange { length },
        );
    }
    assert_error(&raw_pair(1, Some(i64::MAX)), 0, 0, RunlistError::Overflow);
    assert_error(
        &raw_pair(2, None),
        i64::MAX - 1,
        i64::MAX - 1,
        RunlistError::Overflow,
    );
    let mut delta_overflow = raw_pair(1, Some(i64::MAX - 1));
    delta_overflow.extend(raw_pair(1, Some(2)));
    delta_overflow.push(0);
    assert_error(&delta_overflow, 0, 1, RunlistError::Overflow);
    assert_error(
        &raw_pair(1, Some(i64::MIN)),
        0,
        0,
        RunlistError::LcnOutOfRange { lcn: i64::MIN },
    );
}

#[test]
fn geometry_checks_accept_sparse_and_exact_end_but_reject_overrun() {
    let geometry = parse_boot_sector(
        &BootSectorBuilder {
            sectors: 128,
            cluster_encoding: 1,
            ..Default::default()
        }
        .build(),
    )
    .unwrap();
    let exact = decode(&[0x11, 1, 127, 0], 0, 0).unwrap()[0];
    assert!(exact.validate_geometry(&geometry).is_ok());
    let over = decode(&[0x11, 2, 127, 0], 0, 1).unwrap()[0];
    assert_eq!(
        over.validate_geometry(&geometry),
        Err(RunlistError::RunOutOfVolume)
    );
    let sparse = decode(&[0x02, 0, 1, 0], 0, 255).unwrap()[0];
    assert!(sparse.validate_geometry(&geometry).is_ok());
}

fn models() -> impl Strategy<Value = Vec<RunModel>> {
    prop::collection::vec((
        prop_oneof![4 => 1u64..1_000_000, 1 => prop::sample::select(vec![1u64, 127, 128, 255, 256, 32767, 32768, 65535, 65536])],
        prop_oneof![1 => Just(None), 3 => prop_oneof![4 => 0u64..1_000_000_000, 1 => prop::sample::select(vec![0u64, 1, 127, 128, 255, 256, 32767, 32768])].prop_map(Some)],
    ).prop_map(|(len,lcn)| RunModel { len,lcn }), 0..=32)
}

fn segment_bounds(models: &[RunModel], lowest: i64) -> (i64, i64) {
    if models.is_empty() {
        return (0, -1);
    }
    let count: i128 = models.iter().map(|run| i128::from(run.len)).sum();
    (
        lowest,
        i64::try_from(i128::from(lowest) + count - 1).unwrap(),
    )
}

proptest! {
    #![proptest_config(support::proptest_config("proptest-regressions/runlist.txt"))]
    #[test]
    fn run_models_round_trip_with_every_padded_width(models in models(), lowest in 0i64..65536) {
        let (lowest, highest) = segment_bounds(&models, lowest);
        let encoder = RunlistEncoder { runs: models.clone() };
        let expected = decode(&encoder.encode(0), lowest, highest).unwrap();
        let mut vcn = lowest as u128;
        for (run, model) in expected.iter().zip(&models) {
            prop_assert_eq!(u128::from(run.vcn()), vcn);
            prop_assert_eq!(run.length(), model.len);
            prop_assert_eq!(run.lcn(), model.lcn);
            vcn += u128::from(model.len);
        }
        prop_assert_eq!(expected.len(), models.len());
        for width in 1..=8 {
            prop_assert_eq!(decode(&encoder.encode(width), lowest, highest).unwrap(), expected.clone());
        }
        let collected: Result<Vec<_>, _> = Runlist::new(&encoder.encode(0), VcnRange::new(lowest, highest).unwrap()).collect();
        prop_assert_eq!(&collected.unwrap(), &expected);
    }

    #[test]
    fn sparse_base_and_segment_resets_match_models(models in models(), lowest in 0i64..65536, a in 0u64..1_000_000, b in 0u64..1_000_000) {
        let sparse_model = vec![RunModel { len: 1, lcn: Some(a) }, RunModel { len: 16, lcn: None }, RunModel { len: 1, lcn: Some(b) }];
        let sparse_bytes = RunlistEncoder { runs: sparse_model }.encode(0);
        let sparse_runs = decode(&sparse_bytes, 0, 17).unwrap();
        prop_assert_eq!(sparse_runs[2].lcn(), Some(b));
        let (lowest, highest) = segment_bounds(&models, lowest);
        let expected = decode(&RunlistEncoder { runs: models.clone() }.encode(0), lowest, highest).unwrap();
        for split in 1..models.len() {
            let split_vcn = i128::from(lowest) + models[..split].iter().map(|run| i128::from(run.len)).sum::<i128>();
            let split_vcn = i64::try_from(split_vcn).unwrap();
            let mut joined = decode(&RunlistEncoder { runs: models[..split].to_vec() }.encode(0), lowest, split_vcn - 1).unwrap();
            joined.extend(decode(&RunlistEncoder { runs: models[split..].to_vec() }.encode(0), split_vcn, highest).unwrap());
            prop_assert_eq!(joined, expected.clone());
        }
    }

    #[test]
    fn padding_is_ignored_and_coverage_must_be_exact(models in models(), padding in prop::collection::vec(any::<u8>(), 0..=256), lowest in 0i64..65536) {
        let (lowest, highest) = segment_bounds(&models, lowest);
        let mut bytes = RunlistEncoder { runs: models.clone() }.encode(0);
        let expected = decode(&bytes, lowest, highest).unwrap();
        bytes.extend(padding);
        prop_assert_eq!(decode(&bytes, lowest, highest).unwrap(), expected);
        for delta in [-2, -1, 1, 2] { prop_assert!(decode(&bytes, lowest, highest + delta).is_err()); }
        bytes.truncate(bytes.len().min(RunlistEncoder { runs: models }.encode(0).len()) - 1);
        prop_assert_eq!(decode(&bytes, lowest, highest), Err(RunlistError::MissingTerminator));
    }

    #[test]
    fn near_limit_overflow_and_negative_lcns_are_rejected(start in (i64::MAX - 1024)..i64::MAX, increase in 1i64..=2048, negative in -2048i64..=-6) {
        let mut bytes = raw_pair(1, Some(start)); bytes.extend(raw_pair(1, Some(increase))); bytes.push(0);
        let end = i128::from(start) + i128::from(increase) + 1;
        prop_assert_eq!(decode(&bytes, 0, 1).is_err(), end > i128::from(i64::MAX));
        let mut bytes = raw_pair(1, Some(5)); bytes.extend(raw_pair(1, Some(negative))); bytes.push(0);
        prop_assert_eq!(decode(&bytes, 0, 1), Err(RunlistError::LcnOutOfRange { lcn: 5 + negative }));
    }

    #[test]
    fn arbitrary_input_is_bounded_and_fused(bytes in prop::collection::vec(any::<u8>(), 0..=65536), lowest in 0i64..=(i64::MAX - 65536), span in 0i64..65536, empty in any::<bool>()) {
        let range = if empty { VcnRange::new(0, -1).unwrap() } else { VcnRange::new(lowest, lowest + span).unwrap() };
        let mut runs = Runlist::new(&bytes, range);
        let bound = bytes.len() / 2 + 1;
        let mut count = 0;
        let mut error_seen = false;
        for item in runs.by_ref() {
            prop_assert!(!error_seen);
            error_seen = item.is_err();
            count += 1;
            prop_assert!(count <= bound);
            if let Ok(run) = item {
                prop_assert!(run.length() > 0);
                prop_assert!(u128::from(run.vcn()) + u128::from(run.length()) <= i64::MAX as u128);
                if let Some(lcn) = run.lcn() { prop_assert!(u128::from(lcn) + u128::from(run.length()) <= i64::MAX as u128); }
            }
        }
        for _ in 0..16 { prop_assert_eq!(runs.next(), None); }
    }
}
