//! Display formatting for byte counts, untrusted terminal text and scan timestamps.
#![forbid(unsafe_code)]
#![deny(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

mod escape;
mod info;
mod time;
mod units;

pub use escape::{Escaped, ExtensionLabel};
pub use info::{InfoFields, VolumeSerial};
pub use time::ScanTime;
pub use units::ByteSize;
