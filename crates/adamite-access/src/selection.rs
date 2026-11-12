//! Shared report selection and percentage conversion for query frontends.

use crate::{DirectoryLookup, ReportError};
use adamite_core::snapshot::{ExtensionStat, KindStat, Snapshot};
use std::borrow::Cow;

/// Allocation share stored as hundredths of one percent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AllocationShare(u32);

impl AllocationShare {
    /// Converts a finite percentage in 0 through 100, rounding decimal places down.
    #[must_use]
    pub fn from_percent(percent: f64) -> Option<Self> {
        if !percent.is_finite() || !(0.0..=100.0).contains(&percent) {
            return None;
        }
        if percent == 0.0 {
            return Some(Self(0));
        }
        // Decimal conversion avoids losing a basis point for inputs such as 0.29.
        let decimal = percent.to_string();
        let (whole, fraction) = decimal.split_once('.').unwrap_or((&decimal, ""));
        let mut basis_points = whole.parse::<u32>().ok()?;
        let mut digits = fraction.bytes();
        for _ in 0..2 {
            let digit = digits.next().unwrap_or(b'0').checked_sub(b'0')?;
            basis_points = basis_points
                .checked_mul(10)?
                .checked_add(u32::from(digit))?;
        }
        Some(Self(basis_points))
    }

    /// Returns the share in basis points, from zero through ten thousand.
    #[must_use]
    pub const fn basis_points(self) -> u32 {
        self.0
    }
}

/// Grouping used by file allocation breakdowns.
#[derive(Debug, Clone, Copy, Default)]
pub enum BreakdownBy {
    /// Filename extension.
    #[default]
    Extension,
    /// Shared file category.
    Kind,
}

/// Selected statistics, borrowing the stored extension index when no scope is supplied.
#[derive(Debug)]
pub enum BreakdownStats<'a> {
    /// Extension totals in allocation order.
    Extensions(Cow<'a, [ExtensionStat]>),
    /// Category totals in allocation order.
    Kinds(Vec<KindStat>),
}

impl<'a> BreakdownStats<'a> {
    /// Selects the stored extension index or computes statistics beneath a directory.
    ///
    /// # Errors
    /// Returns a directory lookup or query error.
    pub fn new(
        snapshot: &'a Snapshot,
        under: Option<&str>,
        by: BreakdownBy,
    ) -> Result<Self, ReportError> {
        if matches!(by, BreakdownBy::Extension) && under.is_none() {
            return Ok(Self::Extensions(Cow::Borrowed(snapshot.ext_stats())));
        }
        let root = snapshot.resolve_directory(under.unwrap_or("/"))?;
        Ok(match by {
            BreakdownBy::Extension => {
                Self::Extensions(Cow::Owned(snapshot.extension_stats_under(root)?))
            }
            BreakdownBy::Kind => Self::Kinds(snapshot.kind_stats(root)?),
        })
    }
}
