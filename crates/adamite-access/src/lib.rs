//! Loads NTFS sources and constructs and renders snapshot reports.
//!
//! [`Source`] detects snapshot files, images and Windows drive paths. Use [`Source::open_snapshot`] to require a saved snapshot, and [`Source::load_observed`] to decode or scan with caller-supplied limits and a progress observer. [`report`] contains the shared `adamite/1` report schema; [`render`] writes text or JSON to a caller-supplied writer.
#![forbid(unsafe_code)]
#![deny(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

mod error;
pub mod render;
pub mod report;
mod source;

pub use error::{DirectoryLookup, ReportError, SourceError};
pub use source::Source;
