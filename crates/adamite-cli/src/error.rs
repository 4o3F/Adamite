use adamite_core::snapshot::{NodeId, PersistError, SnapshotError};
use adamite_io::error::VolumeError;
use adamite_tui::TuiError;
use snafu::Snafu;
use std::{
    collections::TryReserveError,
    error::Error,
    io::{self, Write},
    path::PathBuf,
};

#[derive(Debug, Snafu)]
pub(crate) enum CliError {
    #[snafu(display("cannot browse snapshot"))]
    Terminal { source: TuiError },
    #[snafu(display("{message}"))]
    Usage { message: &'static str },
    #[snafu(display("path component {component:?} not found in {path:?}"))]
    PathNotFound { path: String, component: String },
    #[snafu(display(
        "ambiguous path component {component:?} in {path:?}; candidate NodeIds: {candidates:?}; use #id to select a direct child"
    ))]
    AmbiguousPath {
        path: String,
        component: String,
        candidates: Vec<NodeId>,
    },
    #[snafu(display("not a directory: {path:?}"))]
    NotADirectory { path: String },
    #[snafu(display("cannot {operation} {}", path.display()))]
    Io {
        operation: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    #[snafu(display("cannot scan {}", path.display()))]
    Volume {
        path: PathBuf,
        drive: bool,
        source: VolumeError,
    },
    #[snafu(display("cannot {operation} snapshot {}", path.display()))]
    Persist {
        operation: &'static str,
        path: PathBuf,
        source: PersistError,
    },
    #[snafu(display("output {} already exists; use --force to replace it", path.display()))]
    OutputExists { path: PathBuf },
    #[snafu(display("output {} is a directory", path.display()))]
    OutputIsDirectory { path: PathBuf },
    #[snafu(display("cannot save {} without replacement: hard links are unsupported; use --force", path.display()))]
    UnsupportedNoClobber { path: PathBuf, source: io::Error },
    #[snafu(display("output {} resolves to the source image", path.display()))]
    SameAsSource { path: PathBuf },
    #[snafu(display("cannot read snapshot tree"))]
    Snapshot { source: SnapshotError },
    #[snafu(display("snapshot is inconsistent: {reason}"))]
    InconsistentSnapshot { reason: &'static str },
    #[snafu(display("snapshot {} is too large for this platform", path.display()))]
    SnapshotLength { path: PathBuf },
    #[snafu(display("cannot allocate snapshot buffer for {}", path.display()))]
    Allocation {
        path: PathBuf,
        source: TryReserveError,
    },
    #[snafu(display("cannot encode JSON report"))]
    Json { source: serde_json::Error },
    #[snafu(display("cannot write standard output"))]
    Output { source: io::Error },
}

impl CliError {
    pub(crate) fn exit_code(&self) -> u8 {
        if matches!(self, Self::Usage { .. }) {
            return 2;
        }
        if matches!(self, Self::Terminal { .. }) {
            return 1;
        }
        let mut current: Option<&(dyn Error + 'static)> = Some(self);
        let mut denied = false;
        while let Some(error) = current {
            if error.is::<PersistError>() {
                return 3;
            }
            denied |= error
                .downcast_ref::<io::Error>()
                .is_some_and(|error| error.kind() == io::ErrorKind::PermissionDenied);
            denied |= matches!(
                error.downcast_ref::<VolumeError>(),
                Some(VolumeError::AccessDenied { .. })
            );
            current = error.source();
        }
        if denied { 4 } else { 1 }
    }

    pub(crate) fn is_broken_pipe(&self) -> bool {
        matches!(self, Self::Output { source } if source.kind() == io::ErrorKind::BrokenPipe)
    }

    pub(crate) fn print(&self) {
        let mut stderr = io::stderr().lock();
        let _ = writeln!(stderr, "error: {self}");
        let mut source = self.source();
        while let Some(cause) = source {
            let _ = writeln!(stderr, "caused by: {cause}");
            source = cause.source();
        }
        if self.exit_code() == 4 && matches!(self, Self::Volume { drive: true, .. }) {
            let _ = writeln!(stderr, "hint: use an elevated terminal to scan a drive");
        }
    }
}
