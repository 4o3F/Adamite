//! Fatal scan failures shared by the driver and processing stages.

use crate::{snapshot::SnapshotError, util::CounterOverflow};
use adamite_ntfs::{
    attribute::{AttributeError, runlist::RunlistError},
    boot::BootError,
    record::{RecordError, fixup::FixupError, reference::FileRefError},
    value::attribute_list::AttributeListError,
};
use snafu::Snafu;
use std::collections::TryReserveError;

/// Failure to advance the scan or issue a valid request.
#[derive(Debug, Snafu)]
#[snafu(visibility(pub(crate)))]
#[non_exhaustive]
pub enum ScanError {
    /// A terminal scan cannot advance again.
    #[snafu(display("scan is inactive"))]
    Inactive,
    /// All issued request slots remain occupied.
    #[snafu(display("outstanding request limit {max_outstanding} reached"))]
    OutstandingLimit {
        /// Configured number of slots.
        max_outstanding: usize,
    },
    /// A range is empty, exceeds the chunk bound or overflows a byte offset.
    #[snafu(display("invalid read range at {offset} with length {len}"))]
    InvalidReadRange {
        /// Requested volume-relative byte offset.
        offset: u64,
        /// Requested byte count.
        len: usize,
    },
    /// Every representable request identifier has been issued.
    #[snafu(display("request identifiers exhausted"))]
    RequestIdExhausted,
    /// Invalid volume geometry or MFT location in the boot sector.
    #[snafu(display("invalid volume geometry: {source}"))]
    Geometry {
        /// Parser geometry failure.
        source: BootError,
    },
    /// Bootstrap record update-sequence protection could not be restored.
    #[snafu(display("invalid bootstrap fixup: {source}"))]
    Fixup {
        /// Parser fixup failure.
        source: FixupError,
    },
    /// A bootstrap record header is invalid.
    #[snafu(display("invalid bootstrap record: {source}"))]
    Record {
        /// Parser record failure.
        source: RecordError,
    },
    /// A bootstrap record contains an invalid attribute header.
    #[snafu(display("invalid bootstrap attribute: {source}"))]
    Attribute {
        /// Parser attribute failure.
        source: AttributeError,
    },
    /// MFT mapping pairs are malformed or terminate with incorrect coverage.
    #[snafu(display("invalid bootstrap runlist: {source}"))]
    Runlist {
        /// Parser runlist failure.
        source: RunlistError,
    },
    /// Attribute-list value or entry decoding failed.
    #[snafu(display("invalid bootstrap attribute list: {source}"))]
    AttributeList {
        /// Parser attribute-list failure.
        source: AttributeListError,
    },
    /// A non-resident list advertises more than the parser's 256 KiB bound.
    #[snafu(display("attribute-list size {size} exceeds 262144 bytes"))]
    AttributeListTooLarge {
        /// Advertised logical value size.
        size: u64,
    },
    /// A discovery pass found no reachable work and bootstrap remains incomplete.
    #[snafu(display("MFT bootstrap is incomplete and discovery cannot progress"))]
    BootstrapIncomplete,
    /// A streamed record ordinal cannot be represented by an NTFS reference.
    #[snafu(display("unrepresentable streamed file reference: {source}"))]
    FileRef {
        /// Parser reference construction failure.
        source: FileRefError,
    },
    /// A resolved entry or parent-index array invariant is invalid.
    #[snafu(display("invalid tree links: {reason}"))]
    Link {
        /// Rejected entry or parent-graph invariant.
        reason: &'static str,
    },
    /// A required filesystem record was absent or corrupt.
    #[snafu(display("required MFT record {record} is missing or corrupt"))]
    RequiredRecord {
        /// Required virtual record ordinal.
        record: u64,
    },
    /// An internal streaming invariant failed.
    #[snafu(display("invalid MFT stream: {reason}"))]
    Stream {
        /// Violated transport or assembly invariant.
        reason: &'static str,
    },
    /// Required record-zero identity, stream or mapping invariants failed.
    #[snafu(display("invalid MFT bootstrap: {reason}"))]
    Bootstrap {
        /// Rejected bootstrap invariant.
        reason: &'static str,
    },
    /// Arithmetic or index conversion overflowed before issuing a read.
    #[snafu(display("overflow computing {operation}"))]
    Overflow {
        /// Calculation that could not be represented.
        operation: &'static str,
    },
    /// A nonempty physical run exceeds the complete volume clusters.
    #[snafu(display("MFT run at LCN {lcn} with {clusters} clusters is outside the volume"))]
    OutOfVolume {
        /// Physical starting cluster.
        lcn: u64,
        /// Physical cluster count.
        clusters: u64,
    },
    /// A byte offset is not covered by the currently known MFT mapping.
    #[snafu(display("unmapped MFT byte offset {offset}"))]
    UnmappedRange {
        /// Missing virtual or physical byte offset.
        offset: u64,
    },
    /// Snapshot construction, aggregation or final ordering failed.
    #[snafu(display("snapshot processing failed: {source}"))]
    Snapshot {
        /// Snapshot construction failure.
        source: SnapshotError,
    },
    /// Scan storage could not be allocated.
    #[snafu(display("scan allocation failed: {source}"))]
    Allocation {
        /// Allocation failure reported by the slot queue.
        source: TryReserveError,
    },
}

impl From<SnapshotError> for ScanError {
    fn from(source: SnapshotError) -> Self {
        Self::Snapshot { source }
    }
}

impl From<TryReserveError> for ScanError {
    fn from(source: TryReserveError) -> Self {
        Self::Allocation { source }
    }
}

impl From<CounterOverflow> for ScanError {
    fn from(_: CounterOverflow) -> Self {
        Self::overflow("scan counter")
    }
}

impl ScanError {
    pub(crate) fn overflow(operation: &'static str) -> Self {
        Self::Overflow { operation }
    }

    pub(crate) fn bootstrap(reason: &'static str) -> Self {
        Self::Bootstrap { reason }
    }

    pub(crate) fn stream(reason: &'static str) -> Self {
        Self::Stream { reason }
    }

    pub(crate) fn link(reason: &'static str) -> Self {
        Self::Link { reason }
    }

    pub(crate) fn link_overflow() -> Self {
        Self::Overflow {
            operation: "tree linking",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ScanError;
    use crate::util::CounterExt;
    use std::{collections::TryReserveError, error::Error};

    #[test]
    fn allocation_conversion_preserves_variant_display_and_source() -> Result<(), Box<dyn Error>> {
        let source = Vec::<u8>::new()
            .try_reserve(usize::MAX)
            .err()
            .ok_or("unrepresentable reservation succeeded")?;
        let message = source.to_string();
        let error = ScanError::from(source);
        assert!(matches!(error, ScanError::Allocation { .. }));
        assert_eq!(
            error.to_string(),
            format!("scan allocation failed: {message}")
        );
        let source = error.source().ok_or("allocation source was lost")?;
        assert!(source.downcast_ref::<TryReserveError>().is_some());
        assert_eq!(source.to_string(), message);
        Ok(())
    }

    #[test]
    fn counter_conversion_preserves_scan_context() -> Result<(), Box<dyn Error>> {
        let mut counter = u64::MAX;
        let error = counter
            .try_bump()
            .map_err(ScanError::from)
            .err()
            .ok_or("overflowing counter succeeded")?;
        assert!(matches!(
            error,
            ScanError::Overflow {
                operation: "scan counter"
            }
        ));
        assert_eq!(error.to_string(), "overflow computing scan counter");
        assert!(error.source().is_none());
        assert_eq!(counter, u64::MAX);
        Ok(())
    }
}
