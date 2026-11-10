//! Scan configuration, request transport and phase transitions.

mod config;
mod protocol;
mod scanner;
mod stats;
mod status;

pub use config::{ConfigError, ScanConfig, ScanContext};
pub use protocol::{FeedError, ReadRequest, RequestId};
pub use scanner::Scanner;
pub use stats::ScanStats;
pub use status::{ScanOutput, ScanPhase, ScanProgress, ScanStep};
