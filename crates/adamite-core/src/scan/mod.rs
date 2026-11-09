//! Scan configuration, request transport and phase transitions.

mod bootstrap;
mod config;
mod ingest;
mod mapping;
mod merge;
mod protocol;
mod scanner;

pub use config::{ConfigError, ScanConfig, ScanContext};
pub use protocol::{FeedError, ReadRequest, RequestId};
pub use scanner::{ScanError, ScanOutput, ScanPhase, ScanProgress, ScanStep, Scanner};
