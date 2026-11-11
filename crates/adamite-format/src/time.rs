use nt_time::FileTime;
use std::fmt;
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

#[cfg(test)]
mod tests {
    use super::ScanTime;

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
