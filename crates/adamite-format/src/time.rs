use nt_time::FileTime;
use std::{fmt, time::SystemTime};
use time::{UtcDateTime, format_description::well_known::Rfc3339};

/// An optional FILETIME timestamp rendered as RFC 3339 UTC with seconds precision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScanTime(
    /// Raw 100-nanosecond ticks since 1601-01-01, or an absent scan timestamp.
    pub Option<u64>,
);

impl ScanTime {
    /// Returns UTC text, or None for an absent timestamp or a value outside time's range.
    #[must_use]
    pub fn rfc3339(&self) -> Option<String> {
        let datetime = UtcDateTime::try_from(FileTime::new(self.0?)).ok()?;
        datetime.replace_nanosecond(0).ok()?.format(&Rfc3339).ok()
    }
}

impl fmt::Display for ScanTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.rfc3339().as_deref().unwrap_or("unknown"))
    }
}

/// A modification age relative to a caller-supplied clock, rendered in seconds, minutes, hours or days.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Age {
    modified: Option<SystemTime>,
    now: SystemTime,
}

impl Age {
    /// Uses `unknown` for missing timestamps and timestamps later than `now`.
    #[must_use]
    pub const fn new(modified: Option<SystemTime>, now: SystemTime) -> Self {
        Self { modified, now }
    }
}

impl fmt::Display for Age {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Some(elapsed) = self
            .modified
            .and_then(|modified| self.now.duration_since(modified).ok())
        else {
            return f.write_str("unknown");
        };
        let seconds = elapsed.as_secs();
        let (value, unit) = match seconds {
            0..60 => (seconds, "s"),
            60..3600 => (seconds / 60, "m"),
            3600..86400 => (seconds / 3600, "h"),
            _ => (seconds / 86400, "d"),
        };
        write!(f, "{value}{unit} ago")
    }
}

#[cfg(test)]
mod tests {
    use super::{Age, ScanTime};
    use std::time::{Duration, SystemTime};

    #[test]
    fn ages_use_whole_units_and_an_explicit_clock() -> Result<(), Box<dyn std::error::Error>> {
        for (seconds, expected) in [
            (0, "0s ago"),
            (45, "45s ago"),
            (59, "59s ago"),
            (60, "1m ago"),
            (3599, "59m ago"),
            (3600, "1h ago"),
            (86399, "23h ago"),
            (86400, "1d ago"),
            (259200, "3d ago"),
        ] {
            let now = SystemTime::UNIX_EPOCH
                .checked_add(Duration::from_secs(seconds))
                .ok_or("clock overflow")?;
            assert_eq!(
                Age::new(Some(SystemTime::UNIX_EPOCH), now).to_string(),
                expected
            );
        }
        assert_eq!(
            Age::new(None, SystemTime::UNIX_EPOCH).to_string(),
            "unknown"
        );
        let future = SystemTime::UNIX_EPOCH
            .checked_add(Duration::from_secs(1))
            .ok_or("clock overflow")?;
        assert_eq!(
            Age::new(Some(future), SystemTime::UNIX_EPOCH).to_string(),
            "unknown"
        );
        Ok(())
    }

    #[test]
    fn filetime_seconds_and_range() {
        assert_eq!(
            ScanTime(Some(0)).rfc3339().as_deref(),
            Some("1601-01-01T00:00:00Z")
        );
        assert_eq!(
            ScanTime(Some(116_444_736_012_345_678)).rfc3339().as_deref(),
            Some("1970-01-01T00:00:01Z")
        );
        assert_eq!(ScanTime(Some(u64::MAX)).rfc3339(), None);
        assert_eq!(ScanTime(None).rfc3339(), None);
        assert_eq!(ScanTime(Some(0)).to_string(), "1601-01-01T00:00:00Z");
        assert_eq!(ScanTime(Some(u64::MAX)).to_string(), "unknown");
        assert_eq!(ScanTime(None).to_string(), "unknown");
    }
}
