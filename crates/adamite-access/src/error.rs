use adamite_core::snapshot::{NodeId, NodeView, PersistError, QueryError, Snapshot, SnapshotError};
use adamite_io::error::VolumeError;
use snafu::Snafu;
use std::{io, path::PathBuf};

/// A failure to detect, decode or scan a source.
#[derive(Debug, Snafu)]
#[snafu(module)]
pub enum SourceError {
    /// Source file I/O failed.
    #[snafu(display("cannot {operation} {}", path.display()))]
    Io {
        /// Operation that failed.
        operation: &'static str,
        /// Path involved in the failure.
        path: PathBuf,
        /// Underlying error.
        source: io::Error,
    },
    /// The snapshot length exceeds this platform’s address space.
    #[snafu(display("snapshot {} is too large for this platform", path.display()))]
    SnapshotLength {
        /// Path involved in the failure.
        path: PathBuf,
    },
    /// Snapshot decoding failed.
    #[snafu(display("cannot {operation} snapshot {}", path.display()))]
    Persist {
        /// Operation that failed.
        operation: &'static str,
        /// Path involved in the failure.
        path: PathBuf,
        /// Underlying error.
        source: PersistError,
    },
    /// An image or raw volume scan failed.
    #[snafu(display("cannot scan {}", path.display()))]
    Volume {
        /// Path involved in the failure.
        path: PathBuf,
        /// Whether the source is a raw drive.
        drive: bool,
        /// Underlying error.
        source: VolumeError,
    },
    /// The source was not selected as a saved snapshot.
    #[snafu(display("source is not a snapshot"))]
    UnsupportedSource,
    /// Raw-volume scanning was requested outside Windows.
    #[snafu(display("raw volumes require Windows"))]
    RawVolumesRequireWindows,
}

/// A failure to construct, serialize or write a report.
#[derive(Debug, Snafu)]
#[snafu(module)]
pub enum ReportError {
    /// The snapshot is missing required report data.
    #[snafu(display("snapshot is inconsistent: {reason}"))]
    InconsistentSnapshot {
        /// Missing snapshot data.
        reason: &'static str,
    },
    /// Report arguments exceed the supported range.
    #[snafu(display("{message}"))]
    Usage {
        /// Argument error message.
        message: &'static str,
    },
    /// A path component could not be resolved.
    #[snafu(display("path component {component:?} not found in {path:?}"))]
    PathNotFound {
        /// Path involved in the failure.
        path: String,
        /// Path component that failed to resolve.
        component: String,
    },
    /// A path component matches multiple children.
    #[snafu(display(
        "ambiguous path component {component:?} in {path:?}; candidate NodeIds: {candidates:?}; use #id to select a direct child"
    ))]
    AmbiguousPath {
        /// Path involved in the failure.
        path: String,
        /// Path component that failed to resolve.
        component: String,
        /// Matching direct child identifiers.
        candidates: Vec<NodeId>,
    },
    /// A resolved path names a file.
    #[snafu(display("not a directory: {path:?}"))]
    NotADirectory {
        /// Path involved in the failure.
        path: String,
    },
    /// A snapshot query failed.
    #[snafu(display("cannot query snapshot"))]
    Query {
        /// Underlying error.
        source: QueryError,
    },
    /// Reading the snapshot tree failed.
    #[snafu(display("cannot read snapshot tree"))]
    Snapshot {
        /// Underlying error.
        source: SnapshotError,
    },
    /// Serializing a JSON report failed.
    #[snafu(display("cannot encode JSON report"))]
    Json {
        /// Underlying error.
        source: serde_json::Error,
    },
    /// Writing report output failed.
    #[snafu(display("cannot write standard output"))]
    Output {
        /// Underlying error.
        source: io::Error,
    },
}

impl From<QueryError> for ReportError {
    fn from(source: QueryError) -> Self {
        match source {
            QueryError::PathNotFound { path, component } => Self::PathNotFound { path, component },
            QueryError::AmbiguousPath {
                path,
                component,
                candidates,
            } => Self::AmbiguousPath {
                path,
                component,
                candidates,
            },
            source => Self::Query { source },
        }
    }
}

/// Snapshot path lookups that require a directory and report files by their canonical path.
pub trait DirectoryLookup {
    /// Resolves `path` and rejects nodes that are not directories.
    ///
    /// # Errors
    /// Returns a lookup or snapshot error, or [`ReportError::NotADirectory`] with the file’s canonical path.
    fn resolve_directory(&self, path: &str) -> Result<NodeId, ReportError>;
}

impl DirectoryLookup for Snapshot {
    fn resolve_directory(&self, path: &str) -> Result<NodeId, ReportError> {
        let id = match self.resolve_path(path) {
            Ok(id) if self.node(id).is_some_and(NodeView::is_dir) => return Ok(id),
            Ok(id) | Err(QueryError::NotADirectory { node: id }) => id,
            Err(source) => return Err(source.into()),
        };
        let path = self
            .path(id)
            .map_err(|source| ReportError::Snapshot { source })?
            .ok_or_else(|| ReportError::PathNotFound {
                path: format!("#{}", id.raw()),
                component: format!("#{}", id.raw()),
            })?;
        Err(ReportError::NotADirectory { path })
    }
}
