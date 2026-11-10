#![allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

//! Raw-unit preservation and agreement with the standard UTF-16 decoder.

use adamite_ntfs::utf16::{Utf16Error, Utf16Le};
use proptest::prelude::*;

fn encode(units: &[u16]) -> Vec<u8> {
    units.iter().flat_map(|unit| unit.to_le_bytes()).collect()
}

fn unit_sequences() -> impl Strategy<Value = Vec<u16>> {
    prop::collection::vec(
        prop_oneof![
            4 => any::<u16>(),
            3 => 0xd800u16..=0xdfff,
            1 => prop::sample::select(vec![0, 1, 0xd7ff, 0xd800, 0xdbff, 0xdc00, 0xdfff, 0xe000, 0xffff]),
        ],
        0..=512,
    )
}

fn strict_model(units: &[u16]) -> Vec<Result<char, Utf16Error>> {
    let mut index = 0u128;
    char::decode_utf16(units.iter().copied())
        .map(|decoded| {
            let start = index;
            index += decoded
                .as_ref()
                .map_or(1, |character| character.len_utf16()) as u128;
            decoded.map_err(|error| Utf16Error::UnpairedSurrogate {
                index: usize::try_from(start).unwrap(),
                unit: error.unpaired_surrogate(),
            })
        })
        .collect()
}

proptest! {
    #[test]
    fn units_and_decoders_match_standard(units in unit_sequences()) {
        let bytes = encode(&units);
        let view = Utf16Le::new(&bytes).unwrap();
        prop_assert_eq!(view.len_units(), units.len());
        prop_assert_eq!(view.units().collect::<Vec<_>>(), units.clone());
        prop_assert_eq!(view.decode_strict().collect::<Vec<_>>(), strict_model(&units));
        let expected: String = char::decode_utf16(units.iter().copied())
            .map(|decoded| decoded.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect();
        prop_assert_eq!(view.decode_lossy().collect::<String>(), expected);
    }

    #[test]
    fn arbitrary_bytes_are_accepted_exactly_when_even(bytes in prop::collection::vec(any::<u8>(), 0..=65536)) {
        let result = Utf16Le::new(&bytes);
        if bytes.len() % 2 == 0 {
            let view = result.unwrap();
            prop_assert_eq!(view.units().count(), bytes.len() / 2);
            prop_assert!(view.decode_strict().count() <= view.len_units());
            prop_assert!(view.decode_lossy().count() <= view.len_units());
        } else {
            prop_assert_eq!(result, Err(Utf16Error::OddLength { len: bytes.len() }));
        }
    }
}

#[test]
fn supplementary_character_and_nul_are_preserved() {
    let bytes = encode(&[0xd83d, 0xde00, 0]);
    let view = Utf16Le::new(&bytes).unwrap();
    assert_eq!(view.units().collect::<Vec<_>>(), [0xd83d, 0xde00, 0]);
    assert_eq!(
        view.decode_strict().collect::<Result<String, _>>().unwrap(),
        "😀\0"
    );
    assert_eq!(view.decode_lossy().collect::<String>(), "😀\0");
}

#[test]
fn unpaired_surrogate_followed_by_ascii() {
    let bytes = encode(&[0xd800, u16::from(b'a')]);
    let view = Utf16Le::new(&bytes).unwrap();
    assert_eq!(
        view.decode_strict().next(),
        Some(Err(Utf16Error::UnpairedSurrogate {
            index: 0,
            unit: 0xd800
        }))
    );
    assert_eq!(view.decode_lossy().collect::<String>(), "\u{fffd}a");
    assert_eq!(view.units().collect::<Vec<_>>(), [0xd800, u16::from(b'a')]);
}

#[test]
fn surrogate_errors_use_unit_indices_after_pairs_and_lookahead() {
    let units = [
        0xd83d,
        0xde00,
        0xd800,
        0xd801,
        u16::from(b'a'),
        0xdc00,
        0xd800,
    ];
    let bytes = encode(&units);
    let view = Utf16Le::new(&bytes).unwrap();
    assert_eq!(
        view.decode_strict().collect::<Vec<_>>(),
        [
            Ok('😀'),
            Err(Utf16Error::UnpairedSurrogate {
                index: 2,
                unit: 0xd800
            }),
            Err(Utf16Error::UnpairedSurrogate {
                index: 3,
                unit: 0xd801
            }),
            Ok('a'),
            Err(Utf16Error::UnpairedSurrogate {
                index: 5,
                unit: 0xdc00
            }),
            Err(Utf16Error::UnpairedSurrogate {
                index: 6,
                unit: 0xd800
            }),
        ]
    );
    assert_eq!(
        view.decode_strict().collect::<Result<String, _>>(),
        Err(Utf16Error::UnpairedSurrogate {
            index: 2,
            unit: 0xd800
        })
    );
}

#[test]
fn unaligned_bytes_empty_input_and_odd_length() {
    let bytes = [0xff, b'a', 0, 0x3d, 0xd8, 0, 0xde];
    let view = Utf16Le::new(&bytes[1..]).unwrap();
    assert_eq!(view.decode_lossy().collect::<String>(), "a😀");
    let empty = Utf16Le::new(&[]).unwrap();
    assert_eq!(empty.len_units(), 0);
    assert_eq!(empty.decode_strict().next(), None);
    assert_eq!(empty.decode_lossy().next(), None);
    assert_eq!(Utf16Le::new(&[0]), Err(Utf16Error::OddLength { len: 1 }));
    assert_eq!(
        empty.decode_strict().collect::<Result<String, _>>(),
        Ok(String::new())
    );
    assert_eq!(empty.decode_lossy().collect::<String>(), "");
}

#[test]
fn iterators_stay_exhausted() {
    let bytes = encode(&[0xd83d, 0xde00, 0xd800]);
    let view = Utf16Le::new(&bytes).unwrap();
    let mut units = view.units();
    assert_eq!(units.len(), 3);
    assert_eq!(units.by_ref().collect::<Vec<_>>(), [0xd83d, 0xde00, 0xd800]);
    assert_eq!(units.len(), 0);
    let mut strict = view.decode_strict();
    assert_eq!(strict.by_ref().count(), 2);
    let mut lossy = view.decode_lossy();
    assert_eq!(lossy.by_ref().count(), 2);
    for _ in 0..16 {
        assert_eq!(units.next(), None);
        assert_eq!(strict.next(), None);
        assert_eq!(lossy.next(), None);
    }
}
