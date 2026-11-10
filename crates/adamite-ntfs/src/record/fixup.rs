//! Two-pass update-sequence fixup with a fixed 512-byte protection stride.

use snafu::{OptionExt, Snafu, ensure};

use crate::util;

/// Update-sequence protection uses 512-byte strides on every sector geometry.
const STRIDE: usize = 512;
/// A 64 KiB FILE record has at most 128 protected trailers.
const MAX_TRAILERS: usize = 128;

/// A successfully fixed record, borrowing the caller's buffer until all views expire.
#[derive(Debug)]
pub struct FixedRecord<'a> {
    bytes: &'a [u8],
}

impl<'a> FixedRecord<'a> {
    /// Borrows the restored record bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &'a [u8] {
        self.bytes
    }
}

/// Validates all metadata and trailers, then restores each trailer in place.
///
/// No bytes are written on error. Any update-sequence number is accepted.
/// Validation order is record size, array count, array offset and extent, then
/// trailer values in increasing stride order. The volume sector size is irrelevant.
///
/// # Errors
///
/// Returns [`FixupError`] for an unsupported record size, invalid array metadata,
/// or a trailer differing from the array's update-sequence number.
/// Size and offset rule failures report [`FixupError::InvalidRecordSize`] and
/// [`FixupError::InvalidOffset`], respectively.
pub fn apply(input: &mut [u8]) -> Result<FixedRecord<'_>, FixupError> {
    ensure!(
        valid_record_size(input.len()),
        InvalidRecordSizeSnafu { len: input.len() }
    );
    let strides = input.len() / STRIDE;
    let expected = strides
        .checked_add(1)
        .and_then(|count| u16::try_from(count).ok())
        .context(TruncatedSnafu)?;
    let count = util::read_u16(input, 6).context(TruncatedSnafu)?;
    ensure!(
        count == expected,
        InvalidCountSnafu {
            expected,
            actual: count
        }
    );
    let offset = util::read_u16(input, 4).context(TruncatedSnafu)?;
    ensure!(valid_usa_offset(offset), InvalidOffsetSnafu { offset });
    let first = util::read_u16(input, 0x14).context(TruncatedSnafu)?;
    let array = util::scaled_range(usize::from(offset), usize::from(count), 2)
        .context(ArrayOutOfBoundsSnafu { offset, count })?;
    ensure!(
        array.end <= 510 && array.end <= usize::from(first) && input.get(array).is_some(),
        ArrayOutOfBoundsSnafu { offset, count }
    );
    let usn = util::read_u16(input, usize::from(offset)).context(TruncatedSnafu)?;
    let mut replacements = [[0u8; 2]; MAX_TRAILERS];
    for (stride, replacement) in replacements.iter_mut().take(strides).enumerate() {
        let tail = util::trailer_offset(stride, STRIDE, 2).context(TruncatedSnafu)?;
        let actual = util::read_u16(input, tail).context(TruncatedSnafu)?;
        ensure!(
            actual == usn,
            TrailerMismatchSnafu {
                stride: u16::try_from(stride).ok().context(TruncatedSnafu)?,
                expected: usn,
                actual
            }
        );
        let entry = util::element_offset(
            usize::from(offset),
            stride.checked_add(1).context(TruncatedSnafu)?,
            2,
        )
        .context(TruncatedSnafu)?;
        *replacement = util::read_u16(input, entry)
            .context(TruncatedSnafu)?
            .to_le_bytes();
    }
    for (sector, replacement) in input
        .as_chunks_mut::<STRIDE>()
        .0
        .iter_mut()
        .zip(replacements.iter())
    {
        if let Some(tail) = sector.last_chunk_mut::<2>() {
            *tail = *replacement;
        }
    }
    Ok(FixedRecord { bytes: input })
}

/// Invalid update-sequence metadata or a torn protected trailer.
///
/// `ScanStats` mapping: `TrailerMismatch` counts as a bad fixup; other variants
/// count as invalid fixup metadata. The core owns these counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Snafu)]
#[snafu(visibility(pub(crate)))]
#[non_exhaustive]
pub enum FixupError {
    /// Record length is not a power of two in 512..=65536.
    #[snafu(display("invalid fixup record size {len}"))]
    InvalidRecordSize {
        /// Supplied byte length.
        len: usize,
    },
    /// The metadata or a protected trailer could not be read.
    #[snafu(display("truncated fixup metadata or trailer"))]
    Truncated,
    /// The array must contain one USN plus one value per 512-byte stride.
    #[snafu(display("update-sequence count {actual} differs from expected {expected}"))]
    InvalidCount {
        /// Required count.
        expected: u16,
        /// Supplied count.
        actual: u16,
    },
    /// The array offset is odd or precedes 0x2A.
    #[snafu(display("invalid update-sequence offset {offset}"))]
    InvalidOffset {
        /// Supplied byte offset.
        offset: u16,
    },
    /// The array overlaps attributes or the first protected trailer.
    #[snafu(display("update-sequence array at {offset} with {count} entries is out of bounds"))]
    ArrayOutOfBounds {
        /// Supplied array offset.
        offset: u16,
        /// Supplied array count.
        count: u16,
    },
    /// The first mismatching stride indicates a torn record.
    #[snafu(display("trailer at stride {stride} is {actual:#06x}, expected {expected:#06x}"))]
    TrailerMismatch {
        /// Zero-based 512-byte stride index.
        stride: u16,
        /// Update-sequence number.
        expected: u16,
        /// Actual trailer value.
        actual: u16,
    },
}

pub(super) fn valid_record_size(len: usize) -> bool {
    len.is_power_of_two() && (512..=65536).contains(&len)
}

pub(super) fn valid_usa_offset(offset: u16) -> bool {
    offset >= 0x2a && util::is_aligned(usize::from(offset), 2)
}
