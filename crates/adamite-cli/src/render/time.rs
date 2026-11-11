use nt_time::FileTime;
use time::{UtcDateTime, format_description::well_known::Rfc3339};

pub(crate) struct ScanTime(pub(crate) Option<u64>);

impl ScanTime {
    pub(crate) fn rfc3339(&self) -> Option<String> {
        let datetime = UtcDateTime::try_from(FileTime::new(self.0?)).ok()?;
        datetime.replace_nanosecond(0).ok()?.format(&Rfc3339).ok()
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
    }
}
