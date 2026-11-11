//! Exact volume reads and a synchronous driver for NTFS scans.
//!
//! [`reader`] defines the exact positional read contract, a borrowed [`reader::SliceReader`], an [`reader::ImageReader`] for image files, and `reader::RawVolumeReader` for aligned, unbuffered Windows volume reads. [`driver::SyncDriver`] queues core requests and serves them in issue order when the scanner waits for input. [`error::VolumeError`] preserves host and core error sources.
//!
//! Sources start at volume byte zero. The caller supplies scan configuration and optional context.
//!
//! [`reader::ImageReader::context()`] supplies the current scan timestamp with no label or free bytes. On Windows, `RawVolumeReader::context()` queries the label, free bytes and scan timestamp. See the [crate README](https://github.com/4o3F/Adamite/blob/main/crates/adamite-io/README.md) for elevation, alignment, unsafe policy and development commands.
//!
//! # Examples
//!
//! A blank sector fails geometry validation after the driver supplies its bytes.
//!
//! ```
//! use adamite_core::{error::ScanError, scan::ScanConfig};
//! use adamite_io::{driver::SyncDriver, error::VolumeError, reader::SliceReader};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let driver = SyncDriver::new(ScanConfig::default())?;
//! let mut reader = SliceReader::new(&[0u8; 512]);
//! assert!(matches!(
//!     driver.run(&mut reader),
//!     Err(VolumeError::Scan { source: ScanError::Geometry { .. } })
//! ));
//! # Ok(())
//! # }
//! ```
#![cfg_attr(not(windows), forbid(unsafe_code))]
#![deny(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

#[cfg(any(windows, test))]
mod align;
pub mod driver;
pub mod error;
pub mod limits;
mod pacer;
pub mod reader;
