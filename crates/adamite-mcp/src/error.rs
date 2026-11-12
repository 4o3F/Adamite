use adamite_access::SourceError;
use snafu::Snafu;
use std::{io, path::PathBuf, sync::Arc};

/// A snapshot request failure with a stable code and an actionable next step.
#[derive(Debug, Clone, Snafu)]
#[non_exhaustive]
pub enum McpError {
    /// Discovery found no snapshot for the requested volume.
    #[snafu(display("no snapshot found for {volume}:"))]
    NoSnapshot {
        /// ASCII volume letter used by the suggested scan command.
        volume: char,
        /// Absolute snapshot publication directory.
        directory: PathBuf,
    },
    /// A file argument does not have an `.adm` extension.
    #[snafu(display("source is not an .adm snapshot: {}", path.display()))]
    UnsupportedSource {
        /// Rejected source path.
        path: PathBuf,
    },
    /// More than one snapshot matches the request.
    #[snafu(display("snapshot selection is ambiguous: {candidates:?}"))]
    AmbiguousSnapshot {
        /// Absolute paths that the caller can pass as an explicit selection.
        candidates: Vec<PathBuf>,
    },
    /// A selected snapshot could not be decoded.
    #[snafu(display("cannot decode snapshot {}: {source}", path.display()))]
    CorruptSnapshot {
        /// Snapshot to replace by rescanning.
        path: PathBuf,
        /// Original access-layer failure, shared with concurrent waiters.
        source: Arc<SourceError>,
    },
    /// The operating system denied access to a snapshot.
    #[snafu(display("permission denied for {}: {source}", path.display()))]
    PermissionDenied {
        /// Inaccessible path.
        path: PathBuf,
        /// Operating-system failure, shared with concurrent waiters.
        source: Arc<io::Error>,
    },
    /// Other I/O or internal synchronization failures.
    #[snafu(display("cannot access {}: {source}", path.display()))]
    Io {
        /// Path involved in the operation, or an empty path for synchronization failures.
        path: PathBuf,
        /// Original failure, shared with concurrent waiters.
        source: Arc<io::Error>,
    },
}

impl McpError {
    /// Returns the stable tool error code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::NoSnapshot { .. } => "no_snapshot",
            Self::UnsupportedSource { .. } => "unsupported_source",
            Self::AmbiguousSnapshot { .. } => "ambiguous_snapshot",
            Self::CorruptSnapshot { .. } => "corrupt_snapshot",
            Self::PermissionDenied { .. } => "permission_denied",
            Self::Io { .. } => "internal_error",
        }
    }

    /// Returns the content of the D8 `next:` line, without its prefix.
    #[must_use]
    pub fn next_step(&self) -> String {
        match self {
            Self::NoSnapshot { volume, directory } => {
                let target = directory.join(format!("{volume}.adm"));
                let force = if target.exists() { " --force" } else { "" };
                format!(
                    "Ask the user, then call scan_volume for {volume}:. Manual fallback: adamite scan {volume}: -o \"{}\"{force}",
                    target.display()
                )
            }
            Self::UnsupportedSource { path } => {
                let target = path.with_extension("adm");
                let force = if target.exists() { " --force" } else { "" };
                format!(
                    "Scan the image: adamite scan \"{}\" -o \"{}\"{force}",
                    path.display(),
                    target.display()
                )
            }
            Self::AmbiguousSnapshot { candidates } => format!(
                "Pass snapshot with one of these paths: {}",
                candidates
                    .iter()
                    .map(|path| format!("\"{}\"", path.display()))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::CorruptSnapshot { path, .. } => format!(
                "Rescan the original volume or image: adamite scan <volume-or-image> -o \"{}\" --force.",
                path.display()
            ),
            Self::PermissionDenied { path, .. } => format!(
                "Check access to \"{}\" or rescan into the snapshot directory.",
                path.display()
            ),
            Self::Io { path, .. } => format!(
                "Check the path and I/O error for \"{}\", then retry.",
                path.display()
            ),
        }
    }

    pub(crate) fn io(path: impl Into<PathBuf>, source: io::Error) -> Self {
        let path = path.into();
        if source.kind() == io::ErrorKind::PermissionDenied {
            Self::PermissionDenied {
                path,
                source: Arc::new(source),
            }
        } else {
            Self::Io {
                path,
                source: Arc::new(source),
            }
        }
    }

    pub(crate) fn source(path: PathBuf, source: SourceError) -> Self {
        match source {
            SourceError::Io { path, source, .. } => Self::io(path, source),
            source @ (SourceError::Persist { .. } | SourceError::SnapshotLength { .. }) => {
                Self::CorruptSnapshot {
                    path,
                    source: Arc::new(source),
                }
            }
            SourceError::UnsupportedSource => Self::UnsupportedSource { path },
            source => Self::io(path, io::Error::other(source)),
        }
    }

    pub(crate) fn poisoned<T>(_: std::sync::PoisonError<T>) -> Self {
        Self::io(
            PathBuf::new(),
            io::Error::other("snapshot synchronization lock poisoned"),
        )
    }
}
