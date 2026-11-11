//! Self-imposed scan transport and worker ceilings.

use adamite_core::scan::{ConfigError, ScanConfig, ScanContext};
use snafu::Snafu;
use std::{
    fmt,
    num::{NonZeroU64, NonZeroUsize},
    str::FromStr,
};

/// Resource ceilings enforced by the scan driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanLimits {
    /// Requested bytes per second; `None` disables pacing.
    pub max_read_bytes_per_sec: Option<NonZeroU64>,
    /// Maximum outstanding core requests. Native synchronous depth remains one.
    pub io_depth: NonZeroUsize,
    /// Maximum request length, validated by core (512 bytes through 16 MiB).
    pub chunk_size: usize,
    /// Worker ceiling. The current synchronous scanner uses one thread for every ceiling.
    pub threads: NonZeroUsize,
}

impl Default for ScanLimits {
    fn default() -> Self {
        ScanProfile::default().limits()
    }
}

impl ScanLimits {
    /// Produces validated core bounds while preserving caller metadata.
    ///
    /// # Errors
    /// Returns core's configuration error for invalid chunks or overflowing bounds.
    pub fn config(self, context: ScanContext) -> Result<ScanConfig, ConfigError> {
        let config = ScanConfig {
            max_outstanding: self.io_depth.get(),
            chunk_size: self.chunk_size,
            context,
        };
        config.validate()?;
        Ok(config)
    }
}

/// Named resource defaults shared by applications and benchmarks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ScanProfile {
    /// 64 MiB/s with 1 MiB requests.
    Background,
    /// 256 MiB/s with 1 MiB requests.
    #[default]
    Balanced,
    /// Uncapped reads with 4 MiB requests.
    Max,
}

impl ScanProfile {
    /// Returns the profile's limits, with one outstanding request and one worker.
    #[must_use]
    pub fn limits(self) -> ScanLimits {
        let (rate, chunk_size) = match self {
            Self::Background => (NonZeroU64::new(64 << 20), 1 << 20),
            Self::Balanced => (NonZeroU64::new(256 << 20), 1 << 20),
            Self::Max => (None, 4 << 20),
        };
        ScanLimits {
            max_read_bytes_per_sec: rate,
            io_depth: NonZeroUsize::MIN,
            chunk_size,
            threads: NonZeroUsize::MIN,
        }
    }
}

impl fmt::Display for ScanProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Background => "background",
            Self::Balanced => "balanced",
            Self::Max => "max",
        })
    }
}

impl FromStr for ScanProfile {
    type Err = ParseProfileError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "background" => Ok(Self::Background),
            "balanced" => Ok(Self::Balanced),
            "max" => Ok(Self::Max),
            _ => Err(ParseProfileError),
        }
    }
}

/// An unknown resource profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Snafu)]
#[snafu(display("expected background, balanced or max"))]
pub struct ParseProfileError;

#[cfg(test)]
mod tests {
    use super::{ScanLimits, ScanProfile};
    use adamite_core::scan::ScanContext;
    use std::{error::Error, num::NonZeroUsize};

    #[test]
    fn profiles_and_core_validation() -> Result<(), Box<dyn Error>> {
        assert_eq!(ScanLimits::default(), ScanProfile::Balanced.limits());
        for (profile, rate, chunk) in [
            (ScanProfile::Background, Some(64 << 20), 1 << 20),
            (ScanProfile::Balanced, Some(256 << 20), 1 << 20),
            (ScanProfile::Max, None, 4 << 20),
        ] {
            assert_eq!(profile.to_string().parse::<ScanProfile>()?, profile);
            let mut limits = profile.limits();
            assert_eq!(limits.max_read_bytes_per_sec.map(|r| r.get()), rate);
            assert_eq!(limits.chunk_size, chunk);
            assert_eq!(limits.io_depth.get(), 1);
            assert_eq!(limits.threads.get(), 1);
            limits.threads = NonZeroUsize::new(64).ok_or("threads")?;
            limits.io_depth = NonZeroUsize::new(3).ok_or("depth")?;
            let config = limits.config(ScanContext::default())?;
            assert_eq!(config.max_outstanding, 3);
            assert_eq!(config.chunk_size, chunk);
            limits.chunk_size = 16 << 20;
            limits.config(ScanContext::default())?;
            for invalid in [0, 513, (16 << 20) + 512] {
                limits.chunk_size = invalid;
                assert!(limits.config(ScanContext::default()).is_err());
            }
        }
        assert!("fast".parse::<ScanProfile>().is_err());
        Ok(())
    }
}
