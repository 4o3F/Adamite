//! Loads NTFS sources, catalogs snapshots, runs scan jobs, publishes snapshots and renders reports.
//!
//! [`Source`] detects snapshot files, images and Windows drive paths. Use [`Source::open_snapshot`] to require a saved snapshot, and [`Source::load_observed`] to decode or scan with caller-supplied limits and a progress observer. [`report`] contains the shared `adamite/1` report schema; [`render`] writes text or JSON to a caller-supplied writer.
//!
//! [`ScanJob`] runs background source loads and publishing drive scans; [`SnapshotCatalog`] discovers snapshots and matches drive letters.
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
mod job_error;
pub mod render;
pub mod report;
mod save;
mod scan_job;
mod scan_job_error;
mod scan_options;
mod snapshot_catalog;
mod source;
mod status_file;

pub use error::{DirectoryLookup, ReportError, SourceError};
pub use source::Source;

pub use elevated_scan::{CompletedScan, ElevatedJob, ElevatedScan, JobState};
pub use job_error::JobError;
pub use save::{PublishMode, SnapshotFile};
pub use scan_job::{JobStatus, ScanJob};
pub use scan_job_error::ScanJobError;
pub use scan_options::{ReadRate, ScanOptions};
pub use snapshot_catalog::SnapshotCatalog;
pub use status_file::StatusFile;
