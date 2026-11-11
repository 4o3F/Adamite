//! Snapshot directory discovery and direct file listings with per-entry errors.

#[cfg(not(windows))]
use etcetera::BaseStrategy;
use snafu::Snafu;
use std::{
    cmp::Ordering,
    ffi::{OsStr, OsString},
    fs, io,
    path::{Path, PathBuf},
    time::SystemTime,
};

/// Failure to locate, create or enumerate the snapshot directory.
#[derive(Debug, Snafu)]
#[non_exhaustive]
pub enum SnapshotDirError {
    /// The running executable's directory could not be located.
    #[cfg(windows)]
    #[snafu(display("cannot locate the adamite executable directory: {source}"))]
    Executable {
        /// Executable path lookup failure.
        source: io::Error,
    },
    /// The platform's base directory could not be located.
    #[cfg(not(windows))]
    #[snafu(display("cannot locate snapshot data directory: {source}"))]
    Base {
        /// Base-directory discovery failure.
        source: etcetera::HomeDirError,
    },
    /// A directory operation failed.
    #[snafu(display("cannot {operation} snapshot directory {}: {source}", path.display()))]
    Io {
        /// Operation being performed.
        operation: &'static str,
        /// Directory being accessed.
        path: PathBuf,
        /// Host failure.
        source: io::Error,
    },
}

/// A directory of saved snapshots, created explicitly when the caller first saves one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotDir {
    path: PathBuf,
}

/// One snapshot candidate, retaining metadata failures instead of aborting the listing.
#[derive(Debug)]
pub struct SnapshotEntry {
    path: Option<PathBuf>,
    stem: Option<OsString>,
    metadata: Result<EntryMetadata, io::Error>,
}

#[derive(Debug)]
struct EntryMetadata {
    byte_len: u64,
    modified: SystemTime,
}

impl SnapshotDir {
    /// Uses a caller-selected directory without creating it.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Returns `snapshots` next to the running executable, so a portable install keeps its data.
    ///
    /// # Errors
    /// Returns [`SnapshotDirError::Executable`] if the executable path cannot be located.
    #[cfg(windows)]
    pub fn default_path() -> Result<PathBuf, SnapshotDirError> {
        let executable =
            std::env::current_exe().map_err(|source| SnapshotDirError::Executable { source })?;
        let directory = executable
            .parent()
            .ok_or_else(|| SnapshotDirError::Executable {
                source: io::Error::other("executable path has no parent directory"),
            })?;
        Ok(directory.join("snapshots"))
    }

    /// Returns the platform data directory followed by `adamite/snapshots`.
    ///
    /// # Errors
    /// Returns [`SnapshotDirError::Base`] if the base directory cannot be located.
    #[cfg(not(windows))]
    pub fn default_path() -> Result<PathBuf, SnapshotDirError> {
        let base =
            etcetera::choose_base_strategy().map_err(|source| SnapshotDirError::Base { source })?;
        Ok(base.data_dir().join("adamite").join("snapshots"))
    }

    /// Borrows the directory path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Creates the directory and any missing parents; an existing directory is accepted.
    ///
    /// # Errors
    /// Returns [`SnapshotDirError::Io`] if directory creation fails.
    pub fn create(&self) -> Result<(), SnapshotDirError> {
        fs::create_dir_all(&self.path).map_err(|source| self.error("create", source))
    }

    /// Returns `<LETTER>.adm` beneath this directory for an ASCII drive letter.
    #[must_use]
    pub fn drive_snapshot(&self, letter: char) -> PathBuf {
        self.path
            .join(format!("{}.adm", letter.to_ascii_uppercase()))
    }

    /// Lists direct `.adm` files, sorted by filename, following symbolic links.
    ///
    /// Extension matching ignores ASCII case. Directories are omitted, and a missing directory returns an empty list. Individual enumeration, metadata and modification-time failures are retained in [`SnapshotEntry::error`]; unnamed enumeration errors sort last.
    ///
    /// # Errors
    /// Returns [`SnapshotDirError::Io`] if the directory itself cannot be opened.
    pub fn entries(&self) -> Result<Vec<SnapshotEntry>, SnapshotDirError> {
        let directory = match fs::read_dir(&self.path) {
            Ok(directory) => directory,
            Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => return Err(self.error("list", source)),
        };
        let mut entries = Vec::new();
        for entry in directory {
            let entry = match entry {
                Ok(entry) => entry,
                Err(source) => {
                    entries.push(SnapshotEntry {
                        path: None,
                        stem: None,
                        metadata: Err(source),
                    });
                    continue;
                }
            };
            let path = entry.path();
            if !path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("adm"))
            {
                continue;
            }
            let metadata = fs::metadata(&path);
            if metadata.as_ref().is_ok_and(|metadata| !metadata.is_file()) {
                continue;
            }
            let metadata = metadata.and_then(|metadata| {
                Ok(EntryMetadata {
                    byte_len: metadata.len(),
                    modified: metadata.modified()?,
                })
            });
            entries.push(SnapshotEntry {
                stem: path.file_stem().map(OsStr::to_owned),
                path: Some(path),
                metadata,
            });
        }
        entries.sort_unstable_by(|left, right| match (left.path(), right.path()) {
            (Some(left), Some(right)) => left.file_name().cmp(&right.file_name()),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        });
        Ok(entries)
    }

    fn error(&self, operation: &'static str, source: io::Error) -> SnapshotDirError {
        SnapshotDirError::Io {
            operation,
            path: self.path.clone(),
            source,
        }
    }
}

impl SnapshotEntry {
    /// Borrows the file path, absent only when enumeration could not identify the entry.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Borrows the filename without its final extension, preserving native encoding.
    #[must_use]
    pub fn file_stem(&self) -> Option<&OsStr> {
        self.stem.as_deref()
    }

    /// Returns the byte length when metadata and modification-time queries succeeded.
    #[must_use]
    pub fn byte_len(&self) -> Option<u64> {
        self.metadata
            .as_ref()
            .ok()
            .map(|metadata| metadata.byte_len)
    }

    /// Returns the last modification time when entry metadata could be read.
    #[must_use]
    pub fn modified(&self) -> Option<SystemTime> {
        self.metadata
            .as_ref()
            .ok()
            .map(|metadata| metadata.modified)
    }

    /// Borrows the individual enumeration, metadata or modification-time failure.
    #[must_use]
    pub fn error(&self) -> Option<&io::Error> {
        self.metadata.as_ref().err()
    }
}
