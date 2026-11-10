//! Checked byte reads, ranges, alignment and fused iterator completion.

use core::ops::Range;

pub(crate) fn range(offset: usize, len: usize) -> Option<Range<usize>> {
    Some(offset..offset.checked_add(len)?)
}

pub(crate) fn element_offset(base: usize, index: usize, width: usize) -> Option<usize> {
    base.checked_add(index.checked_mul(width)?)
}

pub(crate) fn scaled_range(offset: usize, count: usize, width: usize) -> Option<Range<usize>> {
    range(offset, count.checked_mul(width)?)
}

pub(crate) fn trailer_offset(index: usize, stride: usize, width: usize) -> Option<usize> {
    index
        .checked_add(1)?
        .checked_mul(stride)?
        .checked_sub(width)
}

pub(crate) fn is_aligned(offset: usize, alignment: usize) -> bool {
    alignment.is_power_of_two()
        && alignment
            .checked_sub(1)
            .is_some_and(|mask| offset & mask == 0)
}

fn read_array<const N: usize>(bytes: &[u8], offset: usize) -> Option<[u8; N]> {
    bytes.get(range(offset, N)?)?.try_into().ok()
}

pub(crate) fn read_u8(bytes: &[u8], offset: usize) -> Option<u8> {
    Some(u8::from_le_bytes(read_array(bytes, offset)?))
}

pub(crate) fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(read_array(bytes, offset)?))
}

pub(crate) fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(read_array(bytes, offset)?))
}

pub(crate) fn read_u64(bytes: &[u8], offset: usize) -> Option<u64> {
    Some(u64::from_le_bytes(read_array(bytes, offset)?))
}

pub(crate) fn read_i8(bytes: &[u8], offset: usize) -> Option<i8> {
    Some(i8::from_le_bytes(read_array(bytes, offset)?))
}

pub(crate) fn read_i64(bytes: &[u8], offset: usize) -> Option<i64> {
    Some(i64::from_le_bytes(read_array(bytes, offset)?))
}

pub(crate) fn read_bit(bytes: &[u8], index: usize) -> Option<bool> {
    let byte = bytes.get(index / 8)?;
    Some(byte & (1u8.checked_shl(u32::try_from(index % 8).ok()?)?) != 0)
}

pub(crate) fn read_unsigned(bytes: &[u8], offset: usize, width: usize) -> Option<u64> {
    if !(1..=8).contains(&width) {
        return None;
    }
    let field = bytes.get(range(offset, width)?)?;
    let mut word = [0; 8];
    for (target, source) in word.iter_mut().zip(field) {
        *target = *source;
    }
    read_u64(&word, 0)
}

pub(crate) fn read_signed(bytes: &[u8], offset: usize, width: usize) -> Option<i64> {
    if !(1..=8).contains(&width) {
        return None;
    }
    let field = bytes.get(range(offset, width)?)?;
    let mut word = [if field.last()? & 0x80 == 0 { 0 } else { 0xff }; 8];
    for (target, source) in word.iter_mut().zip(field) {
        *target = *source;
    }
    read_i64(&word, 0)
}

pub(crate) fn finish_step<T, E>(
    step: Result<Option<T>, E>,
    finished: &mut bool,
) -> Option<Result<T, E>> {
    if !matches!(&step, Ok(Some(_))) {
        *finished = true;
    }
    step.transpose()
}

#[cfg(test)]
#[allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]
mod tests {
    use super::*;

    #[test]
    fn little_endian_reads_accept_unaligned_data() {
        let input = [0xff, 0x80, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x87];
        assert_eq!(read_u8(&input, 1), Some(0x80));
        assert_eq!(read_i8(&input, 1), Some(-128));
        assert_eq!(read_u16(&input, 1), Some(0x0180));
        assert_eq!(read_u32(&input, 1), Some(0x0302_0180));
        assert_eq!(read_u64(&input, 1), Some(0x8706_0504_0302_0180));
        assert_eq!(
            read_i64(&input, 1),
            Some(i64::from_le_bytes(input[1..].try_into().unwrap()))
        );
    }

    #[test]
    fn reads_reject_truncation_and_offset_overflow() {
        let input = [0; 8];
        for offset in [8, 9, usize::MAX] {
            assert_eq!(read_u8(&input, offset), None);
            assert_eq!(read_i8(&input, offset), None);
            assert_eq!(read_u16(&input, offset), None);
            assert_eq!(read_u32(&input, offset), None);
            assert_eq!(read_u64(&input, offset), None);
            assert_eq!(read_i64(&input, offset), None);
        }
        assert_eq!(read_u16(&input, 7), None);
        assert_eq!(read_u32(&input, 5), None);
        assert_eq!(read_u64(&input, 1), None);
        assert_eq!(read_i64(&input, 1), None);
    }

    #[test]
    fn ranges_and_alignment_are_checked_at_boundaries() {
        assert_eq!(read_bit(&[0b0000_0101], 0), Some(true));
        assert_eq!(read_bit(&[0b0000_0101], 1), Some(false));
        assert_eq!(read_bit(&[0b0000_0101], 2), Some(true));
        assert_eq!(read_bit(&[], usize::MAX), None);
        assert_eq!(read_unsigned(&[0x80], 0, 1), Some(128));
        assert_eq!(read_signed(&[0x80], 0, 1), Some(-128));
        assert_eq!(read_signed(&[0, 0x80], 0, 2), Some(-32768));
        assert_eq!(read_signed(&i64::MIN.to_le_bytes(), 0, 8), Some(i64::MIN));
        assert_eq!(read_unsigned(&u64::MAX.to_le_bytes(), 0, 8), Some(u64::MAX));
        for width in [0, 9, usize::MAX] {
            assert_eq!(read_unsigned(&[0; 8], 0, width), None);
            assert_eq!(read_signed(&[0; 8], 0, width), None);
        }
        assert_eq!(element_offset(42, 3, 2), Some(48));
        assert_eq!(element_offset(usize::MAX, 1, 2), None);
        assert_eq!(scaled_range(42, 3, 2), Some(42..48));
        assert_eq!(scaled_range(0, usize::MAX, 2), None);
        assert_eq!(trailer_offset(1, 512, 2), Some(1022));
        assert_eq!(trailer_offset(usize::MAX, 512, 2), None);
        assert_eq!(trailer_offset(0, 1, 2), None);
        assert_eq!(range(usize::MAX, 0), Some(usize::MAX..usize::MAX));
        assert_eq!(range(usize::MAX, 1), None);
        assert_eq!(range(5, 3), Some(5..8));
        for alignment in [1, 2, 8, 512] {
            assert!(is_aligned(0, alignment));
            assert!(is_aligned(alignment, alignment));
        }
        assert!(!is_aligned(3, 2));
        for alignment in [0, 3, usize::MAX] {
            assert!(!is_aligned(0, alignment));
        }
    }
}
