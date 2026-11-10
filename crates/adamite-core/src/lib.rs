//! Single-threaded NTFS scans from caller-supplied bytes into immutable snapshots and diagnostic counters.
//!
//! [`scan::Scanner::step`] emits volume-relative [`scan::ReadRequest`] values; [`scan::Scanner::feed`] copies exact responses. Core performs no I/O, defines no reader trait or callback, and starts no threads or timers. The caller owns reads, waiting and optional [`scan::ScanContext`] metadata.
//!
//! [`scan::ScanConfig::default`] selects one outstanding request and 1 MiB chunks. Valid chunks are multiples of 512 from 512 bytes to 16 MiB; cap must be positive and cap × chunk must fit `usize`. Responses may arrive in any order, but slots stay occupied until consumed in virtual order. Transport storage is bounded separately from metadata storage, which grows with the scan.
//!
//! # Examples
//!
//! This synchronous adapter serves every request from an in-memory image. The doctest passes a blank sector and checks geometry rejection; a valid complete image produces [`scan::ScanOutput`]. Asynchronous adapters may queue reads and wait when [`scan::ScanProgress::waiting_for_input`] is true.
//!
//! ```
//! use adamite_core::{
//!     error::ScanError,
//!     scan::{ScanConfig, ScanOutput, ScanStep, Scanner},
//! };
//! use std::error::Error;
//!
//! fn scan_image(image: &[u8]) -> Result<ScanOutput, Box<dyn Error + Send + Sync>> {
//!     let mut scanner = Scanner::new(ScanConfig::default())?;
//!     loop {
//!         match scanner.step()? {
//!             ScanStep::Need(request) => {
//!                 let start = usize::try_from(request.offset)?;
//!                 let end = start.checked_add(request.len).ok_or("read range overflow")?;
//!                 let bytes = image.get(start..end).ok_or("image is truncated")?;
//!                 scanner.feed(request.id, bytes)?;
//!             }
//!             ScanStep::Progress(_) => {} // Each read above is answered immediately.
//!             ScanStep::Done(output) => return Ok(output),
//!             _ => return Err("unsupported scan step".into()),
//!         }
//!     }
//! }
//! # fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
//! # let image = [0u8; 512];
//! # let error = scan_image(&image).err().ok_or("blank sector was accepted")?;
//! # assert!(matches!(error.downcast_ref::<ScanError>(), Some(ScanError::Geometry { .. })));
//! # Ok(())
//! # }
//! ```
//!
//! [`scan::ScanStep::Done`] transfers snapshot and stats exactly once; subsequent calls return an inactive error. [`snapshot::Snapshot`] is Send + Sync and may be shared through `std::sync::Arc`. Its node, children and name accessors borrow immutable data; [`snapshot::Snapshot::path`] constructs a path iteratively.
//!
//! # Errors
//!
//! - [`scan::ConfigError`]: invalid transport bounds, before the scanner is created.
//! - [`scan::FeedError`]: inactive/unknown/duplicate ids, wrong response lengths or allocation failure; rejected feeds preserve state and may be corrected.
//! - [`error::ScanError`]: fatal geometry/bootstrap, required-root, arithmetic or allocation failures terminate the scan. Ordinary corrupt records are discarded and counted in [`scan::ScanStats`].
//! - [`snapshot::SnapshotError`]: allocation, representational bounds or tree validation during path access/construction; construction failures are wrapped by [`error::ScanError::Snapshot`].
//!
//! # Determinism and accounting
//!
//! For identical bytes and context, snapshot contents and stats are independent of chunk size, request cap and response order. Stable ids and explicit name/FileRef/id tie-breaks determine child and top-file order. Logical totals count unnamed DATA once per primary link; ADS is separate, while allocation includes every non-resident stream. Lossy names sort by their pooled replacement text during finalization.
//!
//! # Scope
//!
//! Device/image I/O belongs to future `adamite-io` adapters. Parallelism, buffer pools, bitmap skipping, general resource limits, dynamic queries, persistence, CLI, MCP, USN refresh and walkdir fallback are deferred. Core does not follow reparse targets, attempt MFT-mirror fallback or provide an atomic live-volume view. Sparse/compressed/encrypted MFT or bootstrap-list storage is rejected.
#![forbid(unsafe_code)]
#![deny(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

pub mod error;
mod graph;
mod mft;
pub mod scan;
pub mod snapshot;
mod util;
