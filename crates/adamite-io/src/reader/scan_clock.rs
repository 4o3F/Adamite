use crate::error::VolumeError;
use nt_time::FileTime;
use std::time::SystemTime;

/// Supplies scan timestamps from the host clock.
pub(super) struct ScanClock;

impl ScanClock {
    /// Returns the current clock as raw FILETIME ticks.
    ///
    /// # Errors
    ///
    /// Returns [`VolumeError::Overflow`] when the clock cannot be represented as FILETIME ticks.
    pub(super) fn now_filetime() -> Result<u64, VolumeError> {
        Ok(FileTime::try_from(SystemTime::now())
            .map_err(|_| VolumeError::Overflow {
                operation: "scan timestamp as FILETIME",
            })?
            .to_raw())
    }
}
