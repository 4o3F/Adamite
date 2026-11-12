//! Loads NTFS sources, publishes snapshots, runs elevated scans and renders reports.
//!
//! [`Source`] detects snapshot files, images and Windows drive paths. Use [`Source::open_snapshot`] to require a saved snapshot, and [`Source::load_observed`] to decode or scan with caller-supplied limits and a progress observer. [`report`] contains the shared `adamite/1` report schema; [`render`] writes text or JSON to a caller-supplied writer.
//!
//! [`SnapshotFile`] publishes completed scans, [`StatusFile`] writes scan outcomes, and [`ElevatedScan`] launches and monitors a child with [`ScanOptions`].
#![forbid(unsafe_code)]
#![deny(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

mod elevated_scan;
mod error;
pub mod render;
pub mod report;
mod save;
mod scan_job_error;
mod scan_options;
mod source;
mod status_file;

pub use error::{DirectoryLookup, ReportError, SourceError};
pub use source::Source;

pub use elevated_scan::{CompletedScan, ElevatedJob, ElevatedScan, JobState};
pub use save::{PublishMode, SnapshotFile};
pub use scan_job_error::ScanJobError;
pub use scan_options::{ReadRate, ScanOptions};
pub use status_file::StatusFile;
