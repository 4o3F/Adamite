//! Borrowed, possibly unaligned UTF-16LE units and character decoding.

use core::{char::DecodeUtf16, iter::FusedIterator, slice::Iter};

use snafu::{Snafu, ensure};

use crate::bytes;

/// A borrowed UTF-16LE byte sequence that preserves NULs and unpaired surrogates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Utf16Le<'a> {
    bytes: &'a [u8],
}

impl<'a> Utf16Le<'a> {
    /// Borrows whole little-endian code units without checking surrogates.
    ///
    /// # Errors
    ///
    /// Returns [`Utf16Error::OddLength`] if the byte length is odd.
    pub fn new(bytes: &'a [u8]) -> Result<Self, Utf16Error> {
        ensure!(
            bytes::is_aligned(bytes.len(), 2),
            OddLengthSnafu { len: bytes.len() }
        );
        Ok(Self { bytes })
    }

    /// Returns the number of code units, including NULs and unpaired surrogates.
    #[must_use]
    pub fn len_units(self) -> usize {
        self.bytes.len() / 2
    }

    /// Iterates the original code units without requiring aligned memory.
    #[must_use]
    pub fn units(self) -> Units<'a> {
        Units {
            chunks: self.bytes.as_chunks::<2>().0.iter(),
        }
    }

    /// Decodes characters and reports each unpaired surrogate with its code-unit index.
    #[must_use]
    pub fn decode_strict(self) -> StrictChars<'a> {
        StrictChars {
            decoder: core::char::decode_utf16(self.units()),
            unit_index: 0,
        }
    }

    /// Decodes characters, replacing each unpaired surrogate with U+FFFD.
    #[must_use]
    pub fn decode_lossy(self) -> LossyChars<'a> {
        LossyChars {
            strict: self.decode_strict(),
        }
    }
}

/// An allocation-free iterator over the original little-endian code units.
#[derive(Debug, Clone)]
pub struct Units<'a> {
    chunks: Iter<'a, [u8; 2]>,
}

impl Iterator for Units<'_> {
    type Item = u16;

    fn next(&mut self) -> Option<Self::Item> {
        Some(u16::from_le_bytes(*self.chunks.next()?))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.chunks.size_hint()
    }
}

impl ExactSizeIterator for Units<'_> {}
impl FusedIterator for Units<'_> {}

/// An allocation-free decoder yielding characters or indexed unpaired-surrogate errors.
#[derive(Debug, Clone)]
pub struct StrictChars<'a> {
    decoder: DecodeUtf16<Units<'a>>,
    unit_index: usize,
}

impl Iterator for StrictChars<'_> {
    type Item = Result<char, Utf16Error>;

    fn next(&mut self) -> Option<Self::Item> {
        let decoded = self.decoder.next()?;
        let index = self.unit_index;
        let consumed = decoded
            .as_ref()
            .map_or(1, |character| character.len_utf16());
        self.unit_index = bytes::range(index, consumed)?.end;
        Some(decoded.map_err(|error| {
            UnpairedSurrogateSnafu {
                index,
                unit: error.unpaired_surrogate(),
            }
            .build()
        }))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.decoder.size_hint()
    }
}

impl FusedIterator for StrictChars<'_> {}

/// An allocation-free decoder replacing unpaired surrogates with U+FFFD.
#[derive(Debug, Clone)]
pub struct LossyChars<'a> {
    strict: StrictChars<'a>,
}

impl Iterator for LossyChars<'_> {
    type Item = char;

    fn next(&mut self) -> Option<Self::Item> {
        self.strict
            .next()
            .map(|decoded| decoded.unwrap_or(char::REPLACEMENT_CHARACTER))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.strict.size_hint()
    }
}

impl FusedIterator for LossyChars<'_> {}

/// Invalid UTF-16 input.
///
/// `ScanStats` mapping: [`Self::OddLength`] counts as a malformed name;
/// [`Self::UnpairedSurrogate`] counts as invalid UTF-16. The core owns these counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Snafu)]
#[snafu(visibility(pub(crate)))]
#[non_exhaustive]
pub enum Utf16Error {
    /// A trailing byte does not form a whole UTF-16 code unit.
    #[snafu(display("UTF-16LE byte length {len} is odd"))]
    OddLength {
        /// The number of bytes supplied.
        len: usize,
    },
    /// A surrogate has no matching partner at this code-unit index.
    #[snafu(display("unpaired UTF-16 surrogate {unit:#06x} at code-unit index {index}"))]
    UnpairedSurrogate {
        /// The zero-based code-unit index of the surrogate.
        index: usize,
        /// The original unpaired surrogate unit.
        unit: u16,
    },
}
