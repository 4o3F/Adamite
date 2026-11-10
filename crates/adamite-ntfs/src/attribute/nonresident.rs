//! Non-resident segment fields and first-segment stream sizes.

use snafu::{OptionExt, ensure};

use super::{
    AttributeError, AttributeFlags, InitializedPastDataSnafu, InvalidMappingPairsSnafu,
    InvalidVcnRangeSnafu, NegativeSizeSnafu, TruncatedHeaderSnafu,
};
use crate::{Runlist, VcnRange, bytes};

/// Stream sizes valid only in the segment whose lowest VCN is zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamSizes {
    allocated: u64,
    data: u64,
    initialized: u64,
    compressed: Option<u64>,
}

impl StreamSizes {
    /// Returns allocated bytes at non-resident header offset 0x28.
    #[must_use]
    pub const fn allocated(self) -> u64 {
        self.allocated
    }
    /// Returns logical data bytes at offset 0x30.
    #[must_use]
    pub const fn data(self) -> u64 {
        self.data
    }
    /// Returns initialized data bytes at offset 0x38.
    #[must_use]
    pub const fn initialized(self) -> u64 {
        self.initialized
    }
    /// Returns the extended size at 0x40 only for compressed or sparse storage.
    #[must_use]
    pub const fn compressed(self) -> Option<u64> {
        self.compressed
    }
}

/// A validated non-resident segment borrowing its mapping-pair bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NonResident<'a> {
    range: VcnRange,
    compression_unit: u8,
    mapping_pairs: &'a [u8],
    sizes: Option<StreamSizes>,
}

impl<'a> NonResident<'a> {
    pub(super) fn parse(input: &'a [u8], flags: AttributeFlags) -> Result<Self, AttributeError> {
        let offset = usize::from(bytes::read_u16(input, 0x20).context(TruncatedHeaderSnafu)?);
        let header_size = if flags.extended_header() { 72 } else { 64 };
        ensure!(offset >= header_size, InvalidMappingPairsSnafu);
        let mapping_pairs = input.get(offset..).context(InvalidMappingPairsSnafu)?;
        let lowest = bytes::read_i64(input, 0x10).context(TruncatedHeaderSnafu)?;
        let highest = bytes::read_i64(input, 0x18).context(TruncatedHeaderSnafu)?;
        let range = VcnRange::new(lowest, highest).map_err(|_| InvalidVcnRangeSnafu.build())?;
        let sizes = if lowest == 0 {
            let allocated = read_size(input, 0x28)?;
            let data = read_size(input, 0x30)?;
            let initialized = read_size(input, 0x38)?;
            let compressed = if flags.extended_header() {
                Some(read_size(input, 0x40)?)
            } else {
                None
            };
            ensure!(initialized <= data, InitializedPastDataSnafu);
            Some(StreamSizes {
                allocated,
                data,
                initialized,
                compressed,
            })
        } else {
            None
        };
        Ok(Self {
            range,
            compression_unit: bytes::read_u8(input, 0x22).context(TruncatedHeaderSnafu)?,
            mapping_pairs,
            sizes,
        })
    }

    /// Returns the segment's lowest VCN at offset 0x10.
    #[must_use]
    pub const fn lowest_vcn(self) -> u64 {
        self.range.lowest_vcn()
    }
    /// Returns the segment's highest VCN at offset 0x18, including empty-stream -1.
    #[must_use]
    pub const fn highest_vcn(self) -> i64 {
        self.range.highest_vcn()
    }
    /// Returns the single compression-unit byte at offset 0x22.
    #[must_use]
    pub const fn compression_unit(self) -> u8 {
        self.compression_unit
    }
    /// Borrows mapping pairs through the end of this attribute, including padding.
    #[must_use]
    pub const fn mapping_pairs(self) -> &'a [u8] {
        self.mapping_pairs
    }
    /// Returns sizes only for the first segment, whose lowest VCN is zero.
    #[must_use]
    pub const fn sizes(self) -> Option<StreamSizes> {
        self.sizes
    }
    /// Starts a fresh fused runlist with this segment's validated VCN range.
    pub fn runs(self) -> Runlist<'a> {
        Runlist::new(self.mapping_pairs, self.range)
    }
}

fn read_size(input: &[u8], offset: usize) -> Result<u64, AttributeError> {
    let value = bytes::read_i64(input, offset).context(TruncatedHeaderSnafu)?;
    u64::try_from(value)
        .ok()
        .context(NegativeSizeSnafu { offset })
}
