//! Caller-driven NTFS scan protocol with bounded outstanding byte-range requests.
//!
//! [`Scanner::step`] issues volume-relative ranges without reading them. The caller
//! supplies each complete response through [`Scanner::feed`], then advances again.
//! With multiple outstanding requests, responses may arrive in any order. A
//! [`ScanStep::Progress`] whose `waiting_for_input` is true asks the caller to wait
//! for its own read operation before polling again.
//!
//! # Examples
//!
//! Feed the first request from caller-owned bytes. This example uses an invalid
//! boot sector to demonstrate how a parser failure terminates the scan.
//!
//! ```
//! use adamite_core::{ScanConfig, ScanError, ScanStep, Scanner};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let mut scanner = Scanner::new(ScanConfig::default())?;
//! let ScanStep::Need(request) = scanner.step()? else {
//!     return Err("expected the initial boot-sector request".into());
//! };
//! assert_eq!((request.offset, request.len), (0, 512));
//! scanner.feed(request.id, &[0; 512])?;
//! assert!(matches!(scanner.step(), Err(ScanError::Geometry { .. })));
//! # Ok(())
//! # }
//! ```
//!
//! Successful tree linking currently returns [`ScanError::PhaseNotImplemented`]
//! for [`ScanPhase::Snapshot`]. Snapshot construction and scan completion are pending.
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
