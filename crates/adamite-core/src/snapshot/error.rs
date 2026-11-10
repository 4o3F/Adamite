//! Snapshot construction and access failures.

use snafu::Snafu;
use std::collections::TryReserveError;

/// Allocation, representational bounds or tree validation prevented snapshot access or construction.
#[must_use]
#[derive(Debug, Snafu)]
#[non_exhaustive]
pub enum SnapshotError {
    /// A stored index or length cannot fit the arena representation.
    #[snafu(display("snapshot {field} exceeds its representational bound"))]
    Overflow {
        /// Index or length being represented.
        field: &'static str,
    },
    /// Fallible storage reservation failed.
    #[snafu(display("snapshot allocation failed: {source}"))]
    Allocation {
        /// Underlying allocation failure.
        source: TryReserveError,
    },
    /// Parent or CSR invariants are invalid.
    #[snafu(display("invalid snapshot tree: {reason}"))]
    InvalidTree {
        /// Invariant that failed.
        reason: &'static str,
    },
}

impl From<TryReserveError> for SnapshotError {
    fn from(source: TryReserveError) -> Self {
        Self::Allocation { source }
    }
}

impl SnapshotError {
    pub(crate) fn overflow(field: &'static str) -> Self {
        Self::Overflow { field }
    }

    pub(crate) fn invalid(reason: &'static str) -> Self {
        Self::InvalidTree { reason }
    }
}

#[cfg(test)]
mod tests {
    use super::SnapshotError;
    use std::{collections::TryReserveError, error::Error};

    #[test]
    fn allocation_conversion_preserves_variant_display_and_source() -> Result<(), Box<dyn Error>> {
        let source = Vec::<u8>::new()
            .try_reserve(usize::MAX)
            .err()
            .ok_or("unrepresentable reservation succeeded")?;
        let message = source.to_string();
        let error = SnapshotError::from(source);
        assert!(matches!(error, SnapshotError::Allocation { .. }));
        assert_eq!(
            error.to_string(),
            format!("snapshot allocation failed: {message}")
        );
        let source = error.source().ok_or("allocation source was lost")?;
        assert!(source.downcast_ref::<TryReserveError>().is_some());
        assert_eq!(source.to_string(), message);
        Ok(())
    }
}
