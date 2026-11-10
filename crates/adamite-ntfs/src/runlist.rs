//! Fused, allocation-free mapping-pair decoding for one non-resident segment.

use core::iter::FusedIterator;

use snafu::{OptionExt, Snafu, ensure};

use crate::{VolumeGeometry, util};

/// A validated inclusive on-disk VCN range, with highest VCN -1 for an empty stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VcnRange {
    lowest: u64,
    highest: i64,
    end: u64,
}

impl VcnRange {
    /// Validates the signed lowest and highest VCN fields of one segment.
    ///
    /// # Errors
    ///
    /// Returns [`RunlistError::InvalidVcnRange`] unless `(lowest, highest)` is
    /// `(0, -1)` or `0 <= lowest <= highest < i64::MAX`.
    pub fn new(lowest: i64, highest: i64) -> Result<Self, RunlistError> {
        ensure!(
            (lowest == 0 && highest == -1)
                || (lowest >= 0 && lowest <= highest && highest < i64::MAX),
            InvalidVcnRangeSnafu { lowest, highest }
        );
        let end = highest
            .checked_add(1)
            .and_then(|value| u64::try_from(value).ok())
            .context(InvalidVcnRangeSnafu { lowest, highest })?;
        Ok(Self {
            lowest: u64::try_from(lowest)
                .ok()
                .context(InvalidVcnRangeSnafu { lowest, highest })?,
            highest,
            end,
        })
    }

    /// Returns the first virtual cluster in this segment.
    #[must_use]
    pub const fn lowest_vcn(self) -> u64 {
        self.lowest
    }
    /// Returns the inclusive highest VCN, or -1 for an empty stream.
    #[must_use]
    pub const fn highest_vcn(self) -> i64 {
        self.highest
    }
    /// Reports whether the range denotes the empty stream `(0, -1)`.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.end == 0
    }
}

/// One nonempty virtual-cluster run, optionally backed by physical clusters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Run {
    vcn: u64,
    length: u64,
    lcn: Option<u64>,
}

impl Run {
    /// Returns the first virtual cluster of the run.
    #[must_use]
    pub const fn vcn(self) -> u64 {
        self.vcn
    }
    /// Returns the positive number of clusters in the run.
    #[must_use]
    pub const fn length(self) -> u64 {
        self.length
    }
    /// Returns the first physical cluster, or None for a sparse run.
    #[must_use]
    pub const fn lcn(self) -> Option<u64> {
        self.lcn
    }

    /// Checks that a physical run fits within the complete volume clusters.
    ///
    /// # Errors
    ///
    /// Returns [`RunlistError::RunOutOfVolume`] if the physical extent exceeds
    /// the volume. Sparse runs always succeed.
    pub fn validate_geometry(self, geometry: &VolumeGeometry) -> Result<(), RunlistError> {
        if let Some(lcn) = self.lcn {
            ensure!(
                lcn.checked_add(self.length)
                    .is_some_and(|end| end <= geometry.total_clusters()),
                RunOutOfVolumeSnafu
            );
        }
        Ok(())
    }
}

/// Mapping-pair iterator with a fresh LCN delta base for each segment.
///
/// An error is yielded once, then iteration ends. The zero terminator verifies
/// exact VCN coverage and ignores all following padding. Exhaust the iterator
/// to observe missing terminators and under-coverage.
#[must_use = "exhaust the runlist to validate termination and VCN coverage"]
#[derive(Debug, Clone)]
pub struct Runlist<'a> {
    input: &'a [u8],
    cursor: usize,
    range: VcnRange,
    next_vcn: u64,
    previous_lcn: i64,
    finished: bool,
}

impl<'a> Runlist<'a> {
    /// Starts one segment at its lowest VCN with previous physical LCN zero.
    pub fn new(input: &'a [u8], range: VcnRange) -> Self {
        Self {
            input,
            cursor: 0,
            range,
            next_vcn: range.lowest,
            previous_lcn: 0,
            finished: false,
        }
    }

    fn read_next(&mut self) -> Result<Option<Run>, RunlistError> {
        let header = util::read_u8(self.input, self.cursor).context(MissingTerminatorSnafu)?;
        if header == 0 {
            ensure!(
                self.next_vcn == self.range.end,
                CoverageMismatchSnafu {
                    expected: self.range.end,
                    actual: self.next_vcn
                }
            );
            return Ok(None);
        }
        let length_width = header & 0x0f;
        let offset_width = header >> 4;
        ensure!(
            (1..=8).contains(&length_width) && offset_width <= 8,
            InvalidFieldWidthsSnafu {
                length: length_width,
                offset: offset_width
            }
        );
        let length_start = util::range(self.cursor, 1).context(TruncatedPairSnafu)?.end;
        let length_range =
            util::range(length_start, usize::from(length_width)).context(TruncatedPairSnafu)?;
        let offset_range =
            util::range(length_range.end, usize::from(offset_width)).context(TruncatedPairSnafu)?;
        ensure!(
            self.input.get(self.cursor..offset_range.end).is_some(),
            TruncatedPairSnafu
        );
        let length = util::read_unsigned(self.input, length_start, usize::from(length_width))
            .context(TruncatedPairSnafu)?;
        ensure!(length > 0, ZeroRunLengthSnafu);
        ensure!(
            length <= i64::MAX as u64,
            RunLengthOutOfRangeSnafu { length }
        );
        let next_vcn = self
            .next_vcn
            .checked_add(length)
            .filter(|&end| end <= i64::MAX as u64)
            .context(OverflowSnafu)?;
        let lcn = if offset_width == 0 {
            None
        } else {
            let delta =
                util::read_signed(self.input, offset_range.start, usize::from(offset_width))
                    .context(TruncatedPairSnafu)?;
            let lcn = self
                .previous_lcn
                .checked_add(delta)
                .context(OverflowSnafu)?;
            ensure!(lcn >= 0, LcnOutOfRangeSnafu { lcn });
            let signed_length = i64::try_from(length).ok().context(OverflowSnafu)?;
            lcn.checked_add(signed_length).context(OverflowSnafu)?;
            Some(lcn)
        };
        ensure!(
            next_vcn <= self.range.end,
            CoverageMismatchSnafu {
                expected: self.range.end,
                actual: next_vcn
            }
        );
        let physical_lcn = match lcn {
            Some(value) => Some(u64::try_from(value).ok().context(OverflowSnafu)?),
            None => None,
        };
        let run = Run {
            vcn: self.next_vcn,
            length,
            lcn: physical_lcn,
        };
        if let Some(lcn) = lcn {
            self.previous_lcn = lcn;
        }
        self.next_vcn = next_vcn;
        self.cursor = offset_range.end;
        Ok(Some(run))
    }
}

impl Iterator for Runlist<'_> {
    type Item = Result<Run, RunlistError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }
        let step = self.read_next();
        crate::util::finish_step(step, &mut self.finished)
    }
}

impl FusedIterator for Runlist<'_> {}

/// Invalid mapping pairs, VCN coverage, or physical extent.
///
/// `ScanStats` mapping: `RunOutOfVolume` counts as an out-of-volume extent;
/// all other variants count as corrupt runlists. The core owns these counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Snafu)]
#[snafu(visibility(pub(crate)))]
#[non_exhaustive]
pub enum RunlistError {
    /// The signed segment bounds violate the empty or nonempty range rules.
    #[snafu(display("invalid VCN range {lowest}..={highest}"))]
    InvalidVcnRange {
        /// Signed lowest VCN.
        lowest: i64,
        /// Signed highest VCN.
        highest: i64,
    },
    /// No zero terminator is present before input ends.
    #[snafu(display("missing runlist terminator"))]
    MissingTerminator,
    /// The header declares unsupported integer widths.
    #[snafu(display("invalid runlist widths: length {length}, offset {offset}"))]
    InvalidFieldWidths {
        /// Length-field bytes.
        length: u8,
        /// Offset-field bytes.
        offset: u8,
    },
    /// A declared field extends beyond the supplied input.
    #[snafu(display("truncated runlist pair"))]
    TruncatedPair,
    /// A run declares no clusters.
    #[snafu(display("zero run length"))]
    ZeroRunLength,
    /// An unsigned length exceeds i64::MAX.
    #[snafu(display("run length {length} exceeds i64::MAX"))]
    RunLengthOutOfRange {
        /// Decoded unsigned length.
        length: u64,
    },
    /// LCN or VCN arithmetic would exceed the signed 64-bit cluster domain.
    #[snafu(display("runlist cluster arithmetic overflow"))]
    Overflow,
    /// An offset delta produces a negative physical cluster number.
    #[snafu(display("negative run LCN {lcn}"))]
    LcnOutOfRange {
        /// Decoded negative LCN.
        lcn: i64,
    },
    /// A run overruns the segment or its terminator leaves a coverage gap.
    #[snafu(display("runlist end VCN {actual} differs from declared end {expected}"))]
    CoverageMismatch {
        /// Declared exclusive end VCN.
        expected: u64,
        /// Actual exclusive end VCN.
        actual: u64,
    },
    /// A physical run extends past the last complete volume cluster.
    #[snafu(display("run extends outside the volume"))]
    RunOutOfVolume,
}
