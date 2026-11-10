//! Scan configuration, request transport and phase transitions.

mod bootstrap;
mod config;
mod discovery;
mod ingest;
mod mapping;
mod merge;
mod protocol;
mod scanner;
mod stats;

pub use config::{ConfigError, ScanConfig, ScanContext};
pub use protocol::{FeedError, ReadRequest, RequestId};
pub use scanner::{ScanOutput, ScanPhase, ScanProgress, ScanStep, Scanner};
pub use stats::ScanStats;
