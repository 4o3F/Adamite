use adamite_access::{ReportError, ScanJobError, SourceError};
use adamite_core::snapshot::{NodeId, PersistError, QueryError, SnapshotError};
use adamite_io::error::VolumeError;
use adamite_tui::TuiError;
use snafu::Snafu;
use std::{
    error::Error,
    io::{self, Write},
    path::PathBuf,
};

#[derive(Debug, Snafu)]
pub(crate) enum CliError {
    #[snafu(display("cannot start profiling"))]
    Profile {
        source: adamite_profile::ProfileError,
    },
    #[snafu(display("cannot browse snapshot"))]
    Terminal { source: TuiError },
    #[snafu(display("{message}"))]
    Usage { message: &'static str },
    #[snafu(display("status file {} already exists", path.display()))]
    StatusExists { path: PathBuf },
    #[snafu(display("cannot report scan status"))]
    Status {
        source: adamite_io::status::StatusError,
    },
    #[snafu(display("elevation was declined"))]
    ElevationDeclined,
    #[snafu(display("cannot run elevated scan"))]
    Elevation {
        source: adamite_io::elevation::ElevationError,
    },
    #[snafu(display("{message}"))]
    ChildFailed { message: String, exit_code: i32 },
    #[snafu(display("elevated scan exited with code {exit_code} without a final status line"))]
    ChildExited { exit_code: u32 },
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
        no_elevate: bool,
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
    #[snafu(display("cannot query snapshot"))]
    Query { source: QueryError },
    #[snafu(display("snapshot is inconsistent: {reason}"))]
    InconsistentSnapshot { reason: &'static str },
    #[snafu(display("snapshot {} is too large for this platform", path.display()))]
    SnapshotLength { path: PathBuf },
    #[snafu(display("cannot encode JSON report"))]
    Json { source: serde_json::Error },
    #[snafu(display("cannot write standard output"))]
    Output { source: io::Error },
}

impl CliError {
    pub(crate) fn exit_code(&self) -> u8 {
        match self {
            Self::ElevationDeclined | Self::Elevation { .. } => return 4,
            Self::ChildFailed { exit_code, .. } => return u8::try_from(*exit_code).unwrap_or(1),
            _ => {}
        }
        if matches!(self, Self::Usage { .. } | Self::StatusExists { .. }) || self.is_pattern_error()
        {
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

    fn is_pattern_error(&self) -> bool {
        matches!(
            self,
            Self::Query {
                source: QueryError::InvalidGlob { .. } | QueryError::PatternTooLong { .. }
            }
        )
    }

    pub(crate) fn is_broken_pipe(&self) -> bool {
        matches!(self, Self::Output { source } if source.kind() == io::ErrorKind::BrokenPipe)
    }

    pub(crate) fn print(&self) {
        let mut stderr = io::stderr().lock();
        let _ = writeln!(stderr, "error: {}", self.message());
        if self.exit_code() == 4 && matches!(self, Self::Volume { drive: true, .. }) {
            let hint = if matches!(
                self,
                Self::Volume {
                    no_elevate: true,
                    ..
                }
            ) {
                "run without --no-elevate to request elevation, or use an elevated terminal"
            } else {
                "use an elevated terminal to scan a drive"
            };
            let _ = writeln!(stderr, "hint: {hint}");
        }
        if self.is_pattern_error() {
            let _ = writeln!(
                stderr,
                "hint: quote globs such as '*.iso', '[ab]*' or '{{x,y}}*'; plain text matches a substring; patterns are limited to 256 bytes"
            );
        }
        if matches!(self, Self::Elevation { .. }) {
            let _ = writeln!(
                stderr,
                "hint: run with --no-elevate in an elevated terminal"
            );
        }
    }

    /// The error and its `caused by:` chain, as printed after `error: `.
    pub(crate) fn message(&self) -> String {
        let mut message = self.to_string();
        let mut source = self.source();
        while let Some(cause) = source {
            message.push_str("\ncaused by: ");
            message.push_str(&cause.to_string());
            source = cause.source();
        }
        message
    }
}

impl From<QueryError> for CliError {
    fn from(source: QueryError) -> Self {
        ReportError::from(source).into()
    }
}

impl From<SourceError> for CliError {
    fn from(error: SourceError) -> Self {
        match error {
            SourceError::Io {
                operation,
                path,
                source,
            } => Self::Io {
                operation,
                path,
                source,
            },
            SourceError::SnapshotLength { path } => Self::SnapshotLength { path },
            SourceError::Persist {
                operation,
                path,
                source,
            } => Self::Persist {
                operation,
                path,
                source,
            },
            SourceError::Volume {
                path,
                drive,
                source,
            } => Self::Volume {
                path,
                drive,
                no_elevate: false,
                source,
            },
            SourceError::UnsupportedSource => Self::Usage {
                message: "source is not a snapshot",
            },
            SourceError::RawVolumesRequireWindows => Self::Usage {
                message: "raw volumes require Windows",
            },
        }
    }
}

impl From<ReportError> for CliError {
    fn from(error: ReportError) -> Self {
        match error {
            ReportError::InconsistentSnapshot { reason } => Self::InconsistentSnapshot { reason },
            ReportError::Usage { message } => Self::Usage { message },
            ReportError::PathNotFound { path, component } => Self::PathNotFound { path, component },
            ReportError::AmbiguousPath {
                path,
                component,
                candidates,
            } => Self::AmbiguousPath {
                path,
                component,
                candidates,
            },
            ReportError::NotADirectory { path } => Self::NotADirectory { path },
            ReportError::Query { source } => Self::Query { source },
            ReportError::Snapshot { source } => Self::Snapshot { source },
            ReportError::Json { source } => Self::Json { source },
            ReportError::Output { source } => Self::Output { source },
        }
    }
}

impl From<ScanJobError> for CliError {
    fn from(error: ScanJobError) -> Self {
        match error {
            ScanJobError::StatusExists { path } => Self::StatusExists { path },
            ScanJobError::Status { source } => Self::Status { source },
            ScanJobError::ElevationDeclined => Self::ElevationDeclined,
            ScanJobError::Elevation { source } => Self::Elevation { source },
            ScanJobError::ChildFailed { message, exit_code } => {
                Self::ChildFailed { message, exit_code }
            }
            ScanJobError::ChildExited { exit_code } => Self::ChildExited { exit_code },
            ScanJobError::Io {
                operation,
                path,
                source,
            } => Self::Io {
                operation,
                path,
                source,
            },
            ScanJobError::Persist {
                operation,
                path,
                source,
            } => Self::Persist {
                operation,
                path,
                source,
            },
            ScanJobError::OutputExists { path } => Self::OutputExists { path },
            ScanJobError::OutputIsDirectory { path } => Self::OutputIsDirectory { path },
            ScanJobError::UnsupportedNoClobber { path, source } => {
                Self::UnsupportedNoClobber { path, source }
            }
            ScanJobError::SameAsSource { path } => Self::SameAsSource { path },
            ScanJobError::SnapshotLength { path } => Self::SnapshotLength { path },
            ScanJobError::Source { source } => source.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::CliError;
    use adamite_access::{ScanJobError, SourceError};
    use adamite_core::snapshot::PersistError;
    use adamite_io::{elevation::ElevationError, status::StatusError};
    use std::{error::Error, io};

    #[test]
    fn scan_job_errors_preserve_existing_messages_and_exit_codes() {
        let cases = [
            (
                ScanJobError::OutputIsDirectory {
                    path: "out.adm".into(),
                },
                "output out.adm is a directory",
                1,
            ),
            (
                ScanJobError::OutputExists {
                    path: "out.adm".into(),
                },
                "output out.adm already exists; use --force to replace it",
                1,
            ),
            (
                ScanJobError::SameAsSource {
                    path: "out.adm".into(),
                },
                "output out.adm resolves to the source image",
                1,
            ),
            (
                ScanJobError::UnsupportedNoClobber {
                    path: "out.adm".into(),
                    source: io::Error::other("unsupported"),
                },
                "cannot save out.adm without replacement: hard links are unsupported; use --force\ncaused by: unsupported",
                1,
            ),
            (
                ScanJobError::StatusExists {
                    path: "status.jsonl".into(),
                },
                "status file status.jsonl already exists",
                2,
            ),
            (
                ScanJobError::Status {
                    source: StatusError::Io {
                        source: io::Error::other("write failed"),
                    },
                },
                "cannot report scan status\ncaused by: status file I/O failed: write failed\ncaused by: write failed",
                1,
            ),
            (
                ScanJobError::Io {
                    operation: "publish snapshot",
                    path: "out.adm".into(),
                    source: io::Error::other("write failed"),
                },
                "cannot publish snapshot out.adm\ncaused by: write failed",
                1,
            ),
            (
                ScanJobError::SnapshotLength {
                    path: "out.adm".into(),
                },
                "snapshot out.adm is too large for this platform",
                1,
            ),
            (
                ScanJobError::Persist {
                    operation: "encode",
                    path: "out.adm".into(),
                    source: PersistError::BadMagic,
                },
                "cannot encode snapshot out.adm\ncaused by: invalid snapshot magic",
                3,
            ),
            (
                ScanJobError::from(ElevationError::Declined),
                "elevation was declined",
                4,
            ),
            (
                ScanJobError::from(ElevationError::Launch {
                    source: io::Error::other("system launch error"),
                }),
                "cannot run elevated scan\ncaused by: failed to launch elevated process: system launch error\ncaused by: system launch error",
                4,
            ),
            (
                ScanJobError::ChildExited { exit_code: 9 },
                "elevated scan exited with code 9 without a final status line",
                1,
            ),
            (
                ScanJobError::from(SourceError::Persist {
                    operation: "decode",
                    path: "out.adm".into(),
                    source: PersistError::BadMagic,
                }),
                "cannot decode snapshot out.adm\ncaused by: invalid snapshot magic",
                3,
            ),
        ];
        for (error, message, exit_code) in cases {
            let display = error.to_string();
            let error = CliError::from(error);
            assert_eq!(error.to_string(), display);
            assert_eq!(error.message(), message);
            assert_eq!(error.exit_code(), exit_code);
        }
        let denied = CliError::from(ScanJobError::Io {
            operation: "publish snapshot",
            path: "out.adm".into(),
            source: io::Error::from(io::ErrorKind::PermissionDenied),
        });
        assert_eq!(denied.exit_code(), 4);
        // Error printing selects the elevation hint by the existing CLI variant.
        let launch = CliError::from(ScanJobError::from(ElevationError::Launch {
            source: io::Error::other("launch failed"),
        }));
        assert!(matches!(launch, CliError::Elevation { .. }));
        assert!(matches!(
            CliError::from(ScanJobError::from(ElevationError::Declined)),
            CliError::ElevationDeclined
        ));
    }

    #[test]
    fn child_failures_preserve_multiline_messages_and_exit_code_fallbacks() {
        for (exit_code, expected) in [
            (0, 0),
            (1, 1),
            (2, 2),
            (3, 3),
            (4, 4),
            (9, 9),
            (255, 255),
            (-1, 1),
            (256, 1),
        ] {
            let error = CliError::from(ScanJobError::ChildFailed {
                message: "child message\ncaused by: child cause".into(),
                exit_code,
            });
            assert!(error.source().is_none());
            assert_eq!(error.message(), "child message\ncaused by: child cause");
            assert_eq!(error.exit_code(), expected);
        }
    }
}
