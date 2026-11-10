//! Non-resident segment fields and first-segment stream sizes.

use super::runlist::{Runlist, VcnRange};

/// Stream sizes valid only in the segment whose lowest VCN is zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamSizes {
    pub(super) allocated: u64,
    pub(super) data: u64,
    pub(super) initialized: u64,
    pub(super) compressed: Option<u64>,
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
    pub(super) range: VcnRange,
    pub(super) compression_unit: u8,
    pub(super) mapping_pairs: &'a [u8],
    pub(super) sizes: Option<StreamSizes>,
}

impl<'a> NonResident<'a> {
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
