//! Exact volume reads and a synchronous driver for NTFS scans.
//!
//! [`reader`] defines the exact positional read contract, a borrowed [`reader::SliceReader`] and an [`reader::ImageReader`] for image files. [`driver::SyncDriver`] queues core requests and serves them in issue order when the scanner waits for input. [`error::VolumeError`] preserves host and core error sources.
//!
//! Sources start at volume byte zero. The caller supplies scan configuration and optional context.
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

pub mod driver;
pub mod error;
pub mod reader;
