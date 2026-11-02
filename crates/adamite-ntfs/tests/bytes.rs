#![allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

//! Boundary checks for the private byte readers and alignment helpers.

#[path = "../src/bytes.rs"]
mod bytes;

#[test]
fn little_endian_reads_accept_unaligned_data() {
    let input = [0xff, 0x80, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x87];
    assert_eq!(bytes::read_u8(&input, 1), Some(0x80));
    assert_eq!(bytes::read_i8(&input, 1), Some(-128));
    assert_eq!(bytes::read_u16(&input, 1), Some(0x0180));
    assert_eq!(bytes::read_u32(&input, 1), Some(0x0302_0180));
    assert_eq!(bytes::read_u64(&input, 1), Some(0x8706_0504_0302_0180));
    assert_eq!(
        bytes::read_i64(&input, 1),
        Some(i64::from_le_bytes(input[1..].try_into().unwrap()))
    );
}

#[test]
fn reads_reject_truncation_and_offset_overflow() {
    let input = [0; 8];
    for offset in [8, 9, usize::MAX] {
        assert_eq!(bytes::read_u8(&input, offset), None);
        assert_eq!(bytes::read_i8(&input, offset), None);
        assert_eq!(bytes::read_u16(&input, offset), None);
        assert_eq!(bytes::read_u32(&input, offset), None);
        assert_eq!(bytes::read_u64(&input, offset), None);
        assert_eq!(bytes::read_i64(&input, offset), None);
    }
    assert_eq!(bytes::read_u16(&input, 7), None);
    assert_eq!(bytes::read_u32(&input, 5), None);
    assert_eq!(bytes::read_u64(&input, 1), None);
    assert_eq!(bytes::read_i64(&input, 1), None);
}

#[test]
fn ranges_and_alignment_are_checked_at_boundaries() {
    assert_eq!(bytes::read_bit(&[0b0000_0101], 0), Some(true));
    assert_eq!(bytes::read_bit(&[0b0000_0101], 1), Some(false));
    assert_eq!(bytes::read_bit(&[0b0000_0101], 2), Some(true));
    assert_eq!(bytes::read_bit(&[], usize::MAX), None);
    assert_eq!(bytes::read_unsigned(&[0x80], 0, 1), Some(128));
    assert_eq!(bytes::read_signed(&[0x80], 0, 1), Some(-128));
    assert_eq!(bytes::read_signed(&[0, 0x80], 0, 2), Some(-32768));
    assert_eq!(
        bytes::read_signed(&i64::MIN.to_le_bytes(), 0, 8),
        Some(i64::MIN)
    );
    assert_eq!(
        bytes::read_unsigned(&u64::MAX.to_le_bytes(), 0, 8),
        Some(u64::MAX)
    );
    for width in [0, 9, usize::MAX] {
        assert_eq!(bytes::read_unsigned(&[0; 8], 0, width), None);
        assert_eq!(bytes::read_signed(&[0; 8], 0, width), None);
    }
    assert_eq!(bytes::element_offset(42, 3, 2), Some(48));
    assert_eq!(bytes::element_offset(usize::MAX, 1, 2), None);
    assert_eq!(bytes::scaled_range(42, 3, 2), Some(42..48));
    assert_eq!(bytes::scaled_range(0, usize::MAX, 2), None);
    assert_eq!(bytes::trailer_offset(1, 512, 2), Some(1022));
    assert_eq!(bytes::trailer_offset(usize::MAX, 512, 2), None);
    assert_eq!(bytes::trailer_offset(0, 1, 2), None);
    assert_eq!(bytes::range(usize::MAX, 0), Some(usize::MAX..usize::MAX));
    assert_eq!(bytes::range(usize::MAX, 1), None);
    assert_eq!(bytes::range(5, 3), Some(5..8));
    for alignment in [1, 2, 8, 512] {
        assert!(bytes::is_aligned(0, alignment));
        assert!(bytes::is_aligned(alignment, alignment));
    }
    assert!(!bytes::is_aligned(3, 2));
    for alignment in [0, 3, usize::MAX] {
        assert!(!bytes::is_aligned(0, alignment));
    }
}
