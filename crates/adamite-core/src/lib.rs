//! Caller-driven NTFS scan protocol with bounded outstanding byte-range requests.
#![forbid(unsafe_code)]
#![deny(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

mod entry;
mod scan;
mod snapshot;
mod stats;
mod tree;

pub use scan::{
    ConfigError, FeedError, ReadRequest, RequestId, ScanConfig, ScanContext, ScanError, ScanOutput,
    ScanPhase, ScanProgress, ScanStep, Scanner,
};
pub use snapshot::Snapshot;
pub use stats::ScanStats;

const _: fn() = || {
    fn assert_send<T: Send>() {}
    assert_send::<ScanStats>();
    assert_send::<ScanOutput>();
    assert_send::<Scanner>();
};
