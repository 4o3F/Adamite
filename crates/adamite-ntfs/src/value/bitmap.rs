//! Allocation-bit views bounded by the caller's logical bit count.

use crate::util;
use core::iter::FusedIterator;
use snafu::{Snafu, ensure};

/// A borrowed LSB-first bitmap with a validated logical bit count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bitmap<'a> {
    bytes: &'a [u8],
    logical_bits: usize,
}

impl<'a> Bitmap<'a> {
    /// Borrows bytes while limiting visibility to the declared logical bits.
    ///
    /// # Errors
    ///
    /// Returns [`BitmapError::LogicalBitsOutOfRange`] if the count exceeds byte capacity.
    pub fn new(bytes: &'a [u8], logical_bits: usize) -> Result<Self, BitmapError> {
        ensure!(
            logical_bits <= bytes.len().saturating_mul(8),
            LogicalBitsOutOfRangeSnafu {
                logical_bits,
                byte_len: bytes.len()
            }
        );
        Ok(Self {
            bytes,
            logical_bits,
        })
    }
    /// Returns the caller's logical bit count.
    #[must_use]
    pub const fn logical_bits(self) -> usize {
        self.logical_bits
    }
    /// Returns one LSB-first bit, or None at or beyond the logical count.
    #[must_use]
    pub fn get(self, index: usize) -> Option<bool> {
        if index >= self.logical_bits {
            return None;
        }
        util::read_bit(self.bytes, index)
    }
    /// Iterates exactly the logical bit count and remains exhausted afterward.
    #[must_use]
    pub fn iter(self) -> BitmapIter<'a> {
        BitmapIter {
            bitmap: self,
            next: 0,
        }
    }
    /// Iterates strictly increasing set-bit indices, excluding padding bits.
    pub fn iter_set(self) -> impl FusedIterator<Item = usize> + 'a {
        self.iter()
            .enumerate()
            .filter_map(|(index, set)| set.then_some(index))
    }
}

/// A fused exact-size iterator over visible allocation bits.
#[derive(Debug, Clone)]
pub struct BitmapIter<'a> {
    bitmap: Bitmap<'a>,
    next: usize,
}
impl Iterator for BitmapIter<'_> {
    type Item = bool;
    fn next(&mut self) -> Option<Self::Item> {
        let bit = self.bitmap.get(self.next)?;
        self.next = util::range(self.next, 1)?.end;
        Some(bit)
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.bitmap.logical_bits.saturating_sub(self.next);
        (remaining, Some(remaining))
    }
}
impl ExactSizeIterator for BitmapIter<'_> {}
impl FusedIterator for BitmapIter<'_> {}

/// A logical bitmap length exceeding the supplied bytes.
///
/// `ScanStats` mapping: every variant counts as an invalid bitmap length.
/// The core owns this counter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Snafu)]
#[non_exhaustive]
pub enum BitmapError {
    /// The logical count exceeds the capacity of the borrowed byte slice.
    #[snafu(display("bitmap logical length {logical_bits} exceeds {byte_len} bytes"))]
    LogicalBitsOutOfRange {
        /// Requested logical count.
        logical_bits: usize,
        /// Supplied byte count.
        byte_len: usize,
    },
}
