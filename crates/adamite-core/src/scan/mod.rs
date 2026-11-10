//! Scan configuration, request transport and phase transitions.

mod config;
mod protocol;
mod scanner;
mod stats;

pub use config::{ConfigError, ScanConfig, ScanContext};
pub use protocol::{FeedError, ReadRequest, RequestId};
pub use scanner::{ScanOutput, ScanPhase, ScanProgress, ScanStep, Scanner};
pub use stats::ScanStats;
