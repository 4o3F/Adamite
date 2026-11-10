//! Two-pass update-sequence fixup with a fixed 512-byte protection stride.

use snafu::{OptionExt, Snafu, ensure};

use crate::util;

/// Update-sequence protection uses 512-byte strides on every sector geometry.
const STRIDE: usize = 512;
/// A 64 KiB FILE record has at most 128 protected trailers.
const MAX_TRAILERS: usize = 128;

/// A successfully fixed record, borrowing the caller's buffer until all views expire.
#[derive(Debug)]
pub(super) struct FixedRecord<'a> {
    bytes: &'a [u8],
}

impl<'a> FixedRecord<'a> {
    /// Borrows the restored record bytes.
    #[must_use]
    pub(super) fn as_bytes(&self) -> &'a [u8] {
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
pub(super) fn apply(input: &mut [u8]) -> Result<FixedRecord<'_>, FixupError> {
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

#[cfg(test)]
#[allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]
mod tests {
    extern crate std;

    use self::std::{boxed::Box, vec};
    use super::{FixupError, apply};
    use adamite_fixtures::builders::RecordBuilder;
    use proptest::prelude::*;

    #[test]
    fn fixup_restores_only_trailers_on_all_supported_sizes_and_usns() {
        for size in [512, 1024, 4096, 65536] {
            for usn in [0, 1, 0xffff, 0xa55a] {
                let builder = RecordBuilder {
                    size,
                    usn,
                    ..Default::default()
                };
                let mut plain = builder.plain();
                for stride in 0..size / 512 {
                    let tail = (stride + 1) * 512 - 2;
                    plain[tail..tail + 2].copy_from_slice(&(stride as u16 ^ usn).to_le_bytes());
                }
                let mut protected = builder.protect(&plain);
                let original = protected.clone();
                let token = apply(&mut protected).unwrap();
                for index in 0..size {
                    let trailer = index % 512 >= 510;
                    assert_eq!(
                        token.as_bytes()[index],
                        if trailer {
                            plain[index]
                        } else {
                            original[index]
                        }
                    );
                }
            }
        }
    }

    #[test]
    fn every_torn_tail_reports_its_stride_without_any_writes() {
        for size in [512, 1024, 4096, 65536] {
            let builder = RecordBuilder {
                size,
                ..Default::default()
            };
            for stride in 0..size / 512 {
                let mut bytes = builder.build();
                builder.corrupt_tail(&mut bytes, stride);
                let before = bytes.clone();
                assert!(
                    matches!(apply(&mut bytes), Err(FixupError::TrailerMismatch { stride: index, .. }) if index == stride as u16)
                );
                assert_eq!(bytes, before);
            }
        }
    }

    #[test]
    fn fixup_metadata_boundaries_and_validation_order() {
        for size in [0, 1, 511, 513, 1000, 65537, 131072] {
            let mut bytes = vec![0; size];
            assert!(matches!(
                apply(&mut bytes),
                Err(FixupError::InvalidRecordSize { .. })
            ));
        }
        for size in [512, 1024, 4096, 65536] {
            let builder = RecordBuilder {
                size,
                ..Default::default()
            };
            let expected = (size / 512 + 1) as u16;
            for count in [0, expected - 1, expected + 1, u16::MAX] {
                let mut bytes = builder.build();
                bytes[6..8].copy_from_slice(&count.to_le_bytes());
                let before = bytes.clone();
                assert!(matches!(
                    apply(&mut bytes),
                    Err(FixupError::InvalidCount { .. })
                ));
                assert_eq!(bytes, before);
            }
            for offset in [
                0,
                0x29,
                0x2a,
                0x2b,
                0x30,
                510 - expected * 2,
                512 - expected * 2,
                510,
                511,
                u16::MAX,
            ] {
                for first in [0x2a, 0x30, 56, 510, 512] {
                    let mut bytes = builder.build();
                    bytes[4..6].copy_from_slice(&offset.to_le_bytes());
                    bytes[20..22].copy_from_slice(&(first as u16).to_le_bytes());
                    let end = u128::from(offset) + u128::from(expected) * 2;
                    let valid =
                        offset >= 0x2a && offset % 2 == 0 && end <= 510 && end <= first as u128;
                    if valid {
                        bytes[usize::from(offset)..end as usize].fill(0);
                        for stride in 0..size / 512 {
                            bytes[(stride + 1) * 512 - 2..(stride + 1) * 512].fill(0);
                        }
                    }
                    let before = bytes.clone();
                    assert_eq!(
                        apply(&mut bytes).is_ok(),
                        valid,
                        "size={size}, offset={offset}, first={first}"
                    );
                    if !valid {
                        assert_eq!(bytes, before);
                    }
                }
            }
        }
    }

    proptest! {
        #![proptest_config(proptest::test_runner::Config { failure_persistence: Some(Box::new(proptest::test_runner::FileFailurePersistence::Direct("proptest-regressions/file_record.txt"))), ..Default::default() })]
        #[test]
        fn arbitrary_fixup_is_total_and_errors_are_atomic(input in prop::collection::vec(any::<u8>(), 0..=65536)) {
            let mut fixed = input.clone();
            if apply(&mut fixed).is_err() { prop_assert_eq!(fixed, input); }
        }

        #[test]
        fn protected_records_round_trip_and_errors_are_atomic(
            size in prop::sample::select(vec![512usize, 1024, 4096, 65536]),
            usn in any::<u16>(), tails in prop::collection::vec(any::<u16>(), 128),
            selected in any::<usize>(), usa30 in any::<bool>(),
        ) {
            let builder = RecordBuilder { size, usn, usa_offset: if usa30 { 0x30 } else { 0x2a }, ..Default::default() };
            let mut plain = builder.plain();
            for (stride, tail) in tails.iter().take(size / 512).enumerate() {
                plain[(stride + 1) * 512 - 2..(stride + 1) * 512].copy_from_slice(&tail.to_le_bytes());
            }
            let mut bytes = builder.protect(&plain);
            let usa_end = usize::from(builder.usa_offset) + (size / 512 + 1) * 2;
            let token = apply(&mut bytes).unwrap();
            prop_assert_eq!(&token.as_bytes()[..usize::from(builder.usa_offset)], &plain[..usize::from(builder.usa_offset)]);
            prop_assert_eq!(&token.as_bytes()[usa_end..], &plain[usa_end..]);
            let mut torn = builder.protect(&plain);
            builder.corrupt_tail(&mut torn, selected % (size / 512));
            let before = torn.clone();
            prop_assert!(apply(&mut torn).is_err());
            prop_assert_eq!(torn, before);
        }
    }
}
