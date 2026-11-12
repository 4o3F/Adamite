//! Adapts access catalog entries and nonblocking jobs to the TUI's home interfaces.

use crate::{cli::LimitArgs, scan_progress_view::ScanProgressView};
use adamite_access::{JobStatus, ScanJob as AccessJob, ScanOptions, SnapshotCatalog};
use adamite_io::snapshot_dir::{SnapshotDir, SnapshotEntry};
use adamite_tui::{Entry, JobProgress, JobState, ScanJob, SnapshotInfo, Sources};
use std::{path::PathBuf, sync::Arc};

pub(crate) struct CliSources {
    options: ScanOptions,
    catalog: Option<SnapshotCatalog>,
    // Keep native paths alongside the display strings returned to the TUI.
    paths: Vec<(String, PathBuf)>,
}

impl CliSources {
    pub(crate) fn new(limits: LimitArgs) -> Self {
        Self::with_dir(
            limits,
            SnapshotDir::default_path().ok().map(SnapshotDir::new),
        )
    }

    pub(crate) fn with_dir(limits: LimitArgs, directory: Option<SnapshotDir>) -> Self {
        Self {
            options: (&limits).into(),
            catalog: directory.map(SnapshotCatalog::new),
            paths: Vec::new(),
        }
    }

    fn snapshot_info(&mut self, entry: SnapshotEntry) -> SnapshotInfo {
        let path = entry
            .path()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default();
        if let Some(native) = entry.path() {
            self.paths.push((path.clone(), native.to_owned()));
        }
        SnapshotInfo {
            name: entry
                .file_stem()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "unreadable snapshot".to_owned()),
            path,
            modified: entry.modified(),
            error: entry.error().map(ToString::to_string),
        }
    }

    fn snapshot_path(&self, info: &SnapshotInfo) -> PathBuf {
        self.paths
            .iter()
            .find(|(display, _)| display == &info.path)
            .map_or_else(|| PathBuf::from(&info.path), |(_, native)| native.clone())
    }
}

impl Sources for CliSources {
    fn entries(&mut self) -> Vec<Entry> {
        self.paths.clear();
        let snapshots = self
            .catalog
            .as_ref()
            .and_then(|catalog| catalog.entries().ok())
            .unwrap_or_default();
        #[cfg(windows)]
        {
            use adamite_io::volume::Volume;
            let mut snapshots = snapshots;
            let mut entries = Vec::new();
            for volume in Volume::list().unwrap_or_default() {
                let snapshot = SnapshotCatalog::claim_drive(&mut snapshots, volume.letter())
                    .map(|entry| self.snapshot_info(entry));
                entries.push(Entry::Drive {
                    letter: volume.letter(),
                    label: volume.label().to_owned(),
                    capacity: volume.capacity(),
                    snapshot,
                });
            }
            entries.extend(
                snapshots
                    .into_iter()
                    .map(|entry| Entry::Snapshot(self.snapshot_info(entry))),
            );
            entries
        }
        #[cfg(not(windows))]
        {
            snapshots
                .into_iter()
                .map(|entry| Entry::Snapshot(self.snapshot_info(entry)))
                .collect()
        }
    }

    fn start(&mut self, entry: &Entry, rescan: bool) -> Box<dyn ScanJob> {
        let job = match entry {
            Entry::Drive {
                snapshot: Some(info),
                ..
            } if !rescan => AccessJob::open(self.snapshot_path(info), self.options.clone(), None),
            Entry::Drive { letter, .. } => AccessJob::scan_drive(
                *letter,
                self.options.clone(),
                self.catalog.as_ref().map(SnapshotCatalog::directory),
            ),
            Entry::Snapshot(info) => {
                AccessJob::open(self.snapshot_path(info), self.options.clone(), None)
            }
        };
        Box::new(JobAdapter(job))
    }

    fn open_path(&mut self, path: &str) -> Box<dyn ScanJob> {
        Box::new(JobAdapter(AccessJob::open(
            PathBuf::from(path),
            self.options.clone(),
            self.catalog.as_ref(),
        )))
    }
}

struct JobAdapter(AccessJob);

impl JobAdapter {
    fn state(job: &AccessJob, status: JobStatus) -> JobState {
        match status {
            JobStatus::Running(progress) => JobState::Running {
                label: format!(
                    "{} {}",
                    if job.is_loading() {
                        "Loading"
                    } else {
                        "Scanning"
                    },
                    job.path().display()
                ),
                progress: progress.map(|report| JobProgress {
                    phase: ScanProgressView::phase_label(report.scan.phase).to_owned(),
                    records: report.scan.records_processed,
                    planned: report.scan.records_planned,
                    bytes: report.bytes_read,
                }),
            },
            JobStatus::Done(output, _) => match Arc::try_unwrap(output) {
                Ok(output) => JobState::Done(output),
                Err(_) => JobState::Failed("scan output is still shared".to_owned()),
            },
            JobStatus::Failed(error) => JobState::Failed(error.message),
        }
    }
}

impl ScanJob for JobAdapter {
    fn poll(&mut self) -> JobState {
        let status = self.0.poll();
        Self::state(&self.0, status)
    }
}

#[cfg(test)]
mod tests;
