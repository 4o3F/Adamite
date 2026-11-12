use adamite_io::limits::{ScanLimits, ScanProfile};
use std::{
    ffi::OsString,
    num::{NonZeroU64, NonZeroUsize},
};

/// Explicit scan choices, retaining omitted limits for child argument forwarding.
#[derive(Debug, Default, Clone)]
pub struct ScanOptions {
    /// Resource profile, or the default balanced profile when omitted.
    pub profile: Option<ScanProfile>,
    /// Explicit read budget and the argument text to forward to a child.
    pub max_read_rate: Option<ReadRate>,
    /// Explicit worker ceiling.
    pub threads: Option<NonZeroUsize>,
    /// Skip requesting administrator access for raw drives.
    pub no_elevate: bool,
}

/// A parsed read budget paired with the caller's original argument text.
#[derive(Debug, Clone)]
pub struct ReadRate {
    argument: String,
    bytes: NonZeroU64,
}

impl ReadRate {
    /// Pairs a validated positive byte rate with its original argument spelling.
    pub fn new(argument: String, bytes: NonZeroU64) -> Self {
        Self { argument, bytes }
    }
}

impl ScanOptions {
    /// Resolves the profile and applies explicitly supplied rate and worker ceilings.
    pub fn limits(&self) -> ScanLimits {
        let mut limits = self.profile.unwrap_or_default().limits();
        if let Some(rate) = &self.max_read_rate {
            limits.max_read_bytes_per_sec = Some(rate.bytes);
        }
        if let Some(threads) = self.threads {
            limits.threads = threads;
        }
        limits
    }

    /// Forwards explicit profile, rate and worker arguments in their CLI order.
    pub fn child_arguments(&self) -> Vec<OsString> {
        let mut args = Vec::new();
        if let Some(profile) = self.profile {
            args.extend([OsString::from("--profile"), profile.to_string().into()]);
        }
        if let Some(rate) = &self.max_read_rate {
            args.extend([
                OsString::from("--max-read-rate"),
                rate.argument.clone().into(),
            ]);
        }
        if let Some(threads) = self.threads {
            args.extend([OsString::from("--threads"), threads.to_string().into()]);
        }
        args
    }
}
