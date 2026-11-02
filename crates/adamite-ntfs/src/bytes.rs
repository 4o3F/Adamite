//! Checked byte ranges, alignment and little-endian reads.

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
