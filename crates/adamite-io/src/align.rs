//! Checked sector ranges and reusable storage for aligned native reads.

use crate::error::VolumeError;
use std::ops::Range;

/// A sector-aligned native range and the requested bytes within it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Span {
    native_offset: u64,
    native_len: usize,
    request_range: Range<usize>,
}

impl Span {
    /// Expands a request to complete logical sectors without truncation.
    ///
    /// # Errors
    ///
    /// Returns `UnsupportedAlignment` with both sizes set to `logical` for zero or non-power-of-two sizes, `UnalignedVolume` for a partial final sector, and `OutOfBounds` for a request or native range past the volume. Unrepresentable arithmetic or conversions, including request-end overflow near `u64::MAX`, return `Overflow` before bounds comparison.
    pub(crate) fn new(
        offset: u64,
        len: usize,
        logical: u32,
        volume_len: u64,
    ) -> Result<Self, VolumeError> {
        if !logical.is_power_of_two() {
            return Err(VolumeError::UnsupportedAlignment {
                logical,
                physical: logical,
            });
        }
        let sector = u64::from(logical);
        if !volume_len.is_multiple_of(sector) {
            return Err(VolumeError::UnalignedVolume {
                volume_len,
                logical,
            });
        }
        let end = u64::try_from(len)
            .ok()
            .and_then(|len| offset.checked_add(len))
            .ok_or(VolumeError::Overflow {
                operation: "aligned request end",
            })?;
        let out_of_bounds = || VolumeError::OutOfBounds {
            offset,
            len,
            volume_len,
        };
        if end > volume_len {
            return Err(out_of_bounds());
        }
        let native_offset = offset
            .checked_rem(sector)
            .and_then(|remainder| offset.checked_sub(remainder))
            .ok_or(VolumeError::Overflow {
                operation: "native read offset",
            })?;
        let native_end = end
            .checked_next_multiple_of(sector)
            .ok_or(VolumeError::Overflow {
                operation: "native read end",
            })?;
        if native_end > volume_len {
            return Err(out_of_bounds());
        }
        let native_len = native_end
            .checked_sub(native_offset)
            .and_then(|len| usize::try_from(len).ok())
            .ok_or(VolumeError::Overflow {
                operation: "native read length",
            })?;
        let start = offset
            .checked_sub(native_offset)
            .and_then(|start| usize::try_from(start).ok())
            .ok_or(VolumeError::Overflow {
                operation: "request slice offset",
            })?;
        let end = start.checked_add(len).ok_or(VolumeError::Overflow {
            operation: "request slice end",
        })?;
        Ok(Self {
            native_offset,
            native_len,
            request_range: start..end,
        })
    }

    /// Returns the volume-relative start of the native read.
    pub(crate) fn native_offset(&self) -> u64 {
        self.native_offset
    }

    /// Returns the checked native buffer length in bytes.
    pub(crate) fn native_len(&self) -> usize {
        self.native_len
    }

    /// Returns the requested byte range within the native buffer.
    pub(crate) fn request_range(&self) -> Range<usize> {
        self.request_range.clone()
    }
}

/// Reusable byte storage with room to align a requested slice.
#[derive(Debug, Default)]
pub(crate) struct AlignedBuf {
    bytes: Vec<u8>,
}

impl AlignedBuf {
    /// Borrows exactly `len` bytes aligned to the physical sector size.
    ///
    /// # Errors
    ///
    /// Returns `UnsupportedAlignment` with both sizes set to `physical` for zero or non-power-of-two sizes, `Allocation` if growth fails, or `Overflow` for unrepresentable sizes, padding or slice bounds.
    pub(crate) fn slice_mut(
        &mut self,
        len: usize,
        physical: u32,
    ) -> Result<&mut [u8], VolumeError> {
        if !physical.is_power_of_two() {
            return Err(VolumeError::UnsupportedAlignment {
                logical: physical,
                physical,
            });
        }
        let alignment = usize::try_from(physical).map_err(|_| VolumeError::Overflow {
            operation: "physical alignment conversion",
        })?;
        let required = alignment
            .checked_sub(1)
            .and_then(|padding| len.checked_add(padding))
            .ok_or(VolumeError::Overflow {
                operation: "aligned buffer length",
            })?;
        if required > self.bytes.len() {
            let additional =
                required
                    .checked_sub(self.bytes.len())
                    .ok_or(VolumeError::Overflow {
                        operation: "aligned buffer growth",
                    })?;
            self.bytes
                .try_reserve_exact(additional)
                .map_err(|source| VolumeError::Allocation { source })?;
            self.bytes.resize(required, 0);
        }
        let padding = self.bytes.as_ptr().align_offset(alignment);
        if padding == usize::MAX {
            return Err(VolumeError::Overflow {
                operation: "aligned buffer padding",
            });
        }
        let end = padding.checked_add(len).ok_or(VolumeError::Overflow {
            operation: "aligned buffer slice end",
        })?;
        self.bytes
            .get_mut(padding..end)
            .ok_or(VolumeError::Overflow {
                operation: "aligned buffer slice bounds",
            })
    }
}

#[cfg(test)]
mod tests {
    use super::{AlignedBuf, Span};
    use crate::error::VolumeError;
    use proptest::{prelude::*, test_runner::TestCaseError};
    use std::error::Error;

    #[test]
    fn span_expands_to_complete_sectors_and_keeps_the_exact_slice() -> Result<(), Box<dyn Error>> {
        let span = Span::new(513, 512, 512, 2048)?;
        assert_eq!(span.native_offset(), 512);
        assert_eq!(span.native_len(), 1024);
        assert_eq!(span.request_range(), 1..513);
        let boot = Span::new(0, 512, 4096, 8192)?;
        assert_eq!(boot.native_offset(), 0);
        assert_eq!(boot.native_len(), 4096);
        assert_eq!(boot.request_range(), 0..512);
        Ok(())
    }

    #[test]
    fn span_accepts_final_sector_and_empty_requests() -> Result<(), Box<dyn Error>> {
        let span = Span::new(4095, 1, 512, 4096)?;
        assert_eq!(span.native_offset(), 3584);
        assert_eq!(span.native_len(), 512);
        assert_eq!(span.request_range(), 511..512);
        for (offset, volume_len) in [(0, 0), (4096, 4096)] {
            let span = Span::new(offset, 0, 512, volume_len)?;
            assert_eq!(span.native_offset(), offset);
            assert_eq!(span.native_len(), 0);
            assert_eq!(span.request_range(), 0..0);
        }
        let span = Span::new(513, 0, 512, 4096)?;
        assert_eq!(span.native_offset(), 512);
        assert_eq!(span.native_len(), 512);
        assert_eq!(span.request_range(), 1..1);
        Ok(())
    }

    #[test]
    fn invalid_sector_sizes_are_rejected() {
        for size in [0, 3, 511, 513, 4095, u32::MAX] {
            assert!(matches!(
                Span::new(0, 0, size, 0),
                Err(VolumeError::UnsupportedAlignment { logical, physical })
                    if logical == size && physical == size
            ));
            assert!(matches!(
                AlignedBuf::default().slice_mut(0, size),
                Err(VolumeError::UnsupportedAlignment { logical, physical })
                    if logical == size && physical == size
            ));
        }
    }

    #[test]
    fn span_checks_overflow_and_bounds_near_u64_max() -> Result<(), Box<dyn Error>> {
        let volume_len = u64::MAX.checked_sub(511).ok_or("volume length underflow")?;
        assert!(matches!(
            Span::new(u64::MAX, 1, 512, volume_len),
            Err(VolumeError::Overflow {
                operation: "aligned request end"
            })
        ));
        assert!(matches!(
            Span::new(volume_len, 1, 512, volume_len),
            Err(VolumeError::OutOfBounds { offset, len: 1, volume_len: actual })
                if offset == volume_len && actual == volume_len
        ));
        let offset = volume_len.checked_sub(1).ok_or("offset underflow")?;
        let span = Span::new(offset, 1, 512, volume_len)?;
        assert_eq!(span.native_len(), 512);
        assert_eq!(span.request_range(), 511..512);
        let offset = u64::MAX.checked_sub(1).ok_or("offset underflow")?;
        let span = Span::new(offset, 1, 1, u64::MAX)?;
        assert_eq!(span.native_offset(), offset);
        assert_eq!(span.native_len(), 1);
        Ok(())
    }

    #[test]
    fn buffer_reports_size_overflow_and_reservation_failure() {
        assert!(matches!(
            AlignedBuf::default().slice_mut(usize::MAX, 512),
            Err(VolumeError::Overflow {
                operation: "aligned buffer length"
            })
        ));
        assert!(matches!(
            AlignedBuf::default().slice_mut(usize::MAX, 1),
            Err(VolumeError::Allocation { .. })
        ));
    }

    #[test]
    fn buffer_reuses_storage_after_growing_and_shrinking() -> Result<(), Box<dyn Error>> {
        for physical in [512, 4096, 65536] {
            let mut buffer = AlignedBuf::default();
            buffer.slice_mut(1, physical)?.fill(7);
            buffer.slice_mut(131072, physical)?.fill(9);
            let address = buffer.slice_mut(131072, physical)?.as_ptr();
            let capacity = buffer.bytes.capacity();
            let allocated_len = buffer.bytes.len();
            let slice = buffer.slice_mut(1, physical)?;
            assert_eq!(slice.as_ptr(), address);
            assert_eq!(slice, [9]);
            assert_eq!(buffer.bytes.capacity(), capacity);
            assert_eq!(buffer.bytes.len(), allocated_len);
            let empty = buffer.slice_mut(0, physical)?;
            assert!(empty.is_empty());
            assert_eq!(empty.as_ptr(), address);
            let slice = buffer.slice_mut(131072, physical)?;
            assert_eq!(slice.as_ptr(), address);
            assert!(slice.iter().all(|byte| *byte == 9));
        }
        Ok(())
    }

    proptest! {
        #[test]
        fn span_covers_the_request_without_passing_the_volume(
            logical in prop::sample::select(vec![512u32, 4096, 65536]),
            sectors in 1u64..1024,
            offset_seed in any::<u64>(),
            len_seed in any::<u64>(),
        ) {
            let volume_len = sectors.checked_mul(u64::from(logical))
                .ok_or_else(|| TestCaseError::fail("volume length overflow"))?;
            let positions = volume_len.checked_add(1)
                .ok_or_else(|| TestCaseError::fail("position count overflow"))?;
            let offset = offset_seed.checked_rem(positions)
                .ok_or_else(|| TestCaseError::fail("zero position count"))?;
            let remaining = volume_len.checked_sub(offset)
                .and_then(|len| len.checked_add(1))
                .ok_or_else(|| TestCaseError::fail("remaining length overflow"))?;
            let len = len_seed.checked_rem(remaining)
                .ok_or_else(|| TestCaseError::fail("zero remaining length"))?;
            let len = usize::try_from(len)
                .map_err(|error| TestCaseError::fail(error.to_string()))?;
            let span = Span::new(offset, len, logical, volume_len)
                .map_err(|error| TestCaseError::fail(error.to_string()))?;
            let native_len = u64::try_from(span.native_len())
                .map_err(|error| TestCaseError::fail(error.to_string()))?;
            let native_end = span.native_offset().checked_add(native_len)
                .ok_or_else(|| TestCaseError::fail("native end overflow"))?;
            let range = span.request_range();
            prop_assert!(span.native_offset() <= offset);
            prop_assert!(span.native_offset().is_multiple_of(u64::from(logical)));
            prop_assert!(native_len.is_multiple_of(u64::from(logical)));
            prop_assert!(native_end <= volume_len);
            prop_assert_eq!(range.end.checked_sub(range.start), Some(len));
            prop_assert!(range.end <= span.native_len());
            let start = u64::try_from(range.start)
                .map_err(|error| TestCaseError::fail(error.to_string()))?;
            prop_assert_eq!(span.native_offset().checked_add(start), Some(offset));
        }

        #[test]
        fn span_rejects_requests_past_the_end_and_partial_final_sectors(
            logical in prop::sample::select(vec![512u32, 4096, 65536]),
            sectors in 0u64..1024,
            extra in 1usize..65536,
        ) {
            let volume_len = sectors.checked_mul(u64::from(logical))
                .ok_or_else(|| TestCaseError::fail("volume length overflow"))?;
            prop_assert!(matches!(
                Span::new(volume_len, extra, logical, volume_len),
                Err(VolumeError::OutOfBounds { offset, len, volume_len: actual })
                    if offset == volume_len && len == extra && actual == volume_len
            ), "request past volume end must fail");
            let unaligned = volume_len.checked_add(1)
                .ok_or_else(|| TestCaseError::fail("unaligned volume length overflow"))?;
            prop_assert!(matches!(
                Span::new(0, 0, logical, unaligned),
                Err(VolumeError::UnalignedVolume { volume_len: actual, logical: size })
                    if actual == unaligned && size == logical
            ), "partial final sector must fail");
        }

        #[test]
        fn buffer_slice_is_aligned_exact_and_inside_its_allocation(
            physical in prop::sample::select(vec![512u32, 4096, 65536]),
            lengths in prop::collection::vec(0usize..131073, 1..8),
        ) {
            let mut buffer = AlignedBuf::default();
            for len in lengths {
                let slice = buffer.slice_mut(len, physical)
                    .map_err(|error| TestCaseError::fail(error.to_string()))?;
                let address = slice.as_ptr() as usize;
                let alignment = usize::try_from(physical)
                    .map_err(|error| TestCaseError::fail(error.to_string()))?;
                prop_assert!(address.is_multiple_of(alignment));
                prop_assert_eq!(slice.len(), len);
                slice.fill(13);
                let start = buffer.bytes.as_ptr() as usize;
                let end = address.checked_add(len)
                    .ok_or_else(|| TestCaseError::fail("slice address overflow"))?;
                let initialized_end = start.checked_add(buffer.bytes.len())
                    .ok_or_else(|| TestCaseError::fail("storage address overflow"))?;
                let allocated_end = start.checked_add(buffer.bytes.capacity())
                    .ok_or_else(|| TestCaseError::fail("allocation address overflow"))?;
                prop_assert!(address >= start);
                prop_assert!(end <= initialized_end);
                prop_assert!(end <= allocated_end);
            }
        }
    }
}
