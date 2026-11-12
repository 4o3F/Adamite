use crate::{McpError, SnapshotCache, snapshot_cache::Fingerprint};
use adamite_access::SnapshotCatalog;
use adamite_core::scan::ScanOutput;
use adamite_format::ScanTime;
use adamite_io::snapshot_dir::{SnapshotDir, SnapshotDirError, SnapshotEntry};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
    time::SystemTime,
};

/// Discovery sources and the retained snapshot cache size.
#[derive(Debug, Clone)]
pub struct SnapshotOptions {
    /// Explicit snapshot files, included even when metadata cannot be read.
    pub snapshots: Vec<PathBuf>,
    /// Directory override; `None` selects and creates [`SnapshotDir::default_path`].
    pub snapshot_dir: Option<PathBuf>,
    /// Retained output count, defaulting to two; zero retains only in-flight results.
    pub cache_capacity: usize,
}

impl Default for SnapshotOptions {
    fn default() -> Self {
        Self {
            snapshots: Vec::new(),
            snapshot_dir: None,
            cache_capacity: 2,
        }
    }
}

/// One metadata-only discovery row, optionally enriched by a cached scan timestamp.
#[derive(Debug, Clone)]
pub struct SnapshotInfo {
    /// Native filename, absent when directory enumeration could not identify the entry.
    pub name: Option<OsString>,
    /// Absolute path, absent only for an unnamed directory enumeration error.
    pub path: Option<PathBuf>,
    /// File length when metadata was readable.
    pub byte_len: Option<u64>,
    /// File modification time when metadata was readable.
    pub modified: Option<SystemTime>,
    /// Scan timestamp from a matching cached output, or unknown before loading.
    pub scan_time: ScanTime,
    /// Per-entry metadata failure, leaving the rest of the listing available.
    pub error: Option<McpError>,
}

/// Refreshes snapshot discovery, resolves selections and owns the blocking load cache.
#[derive(Debug)]
pub struct SnapshotStore {
    catalog: SnapshotCatalog,
    explicit: Vec<PathBuf>,
    ignore_directory_errors: bool,
    cache: SnapshotCache,
}

impl SnapshotStore {
    /// Resolves absolute source paths and creates the default directory at startup.
    ///
    /// A default-directory creation failure leaves its discovery empty; later calls
    /// retry discovery. An overridden directory is used without creating it.
    ///
    /// # Errors
    /// Returns failures locating the default directory or resolving absolute paths.
    pub fn new(options: SnapshotOptions) -> Result<Self, McpError> {
        let use_default = options.snapshot_dir.is_none();
        let directory = match options.snapshot_dir {
            Some(path) => path,
            None => SnapshotDir::default_path().map_err(Self::directory_error)?,
        };
        let directory = SnapshotDir::new(Self::absolute(&directory)?);
        let ignore_directory_errors = use_default && directory.create().is_err();
        let explicit = options
            .snapshots
            .iter()
            .map(|path| Self::absolute(path))
            .collect::<Result<_, _>>()?;
        Ok(Self {
            catalog: SnapshotCatalog::new(directory),
            explicit,
            ignore_directory_errors,
            cache: SnapshotCache::new(options.cache_capacity),
        })
    }

    /// Borrows the absolute directory used by discovery and suggested scan commands.
    #[must_use]
    pub fn directory(&self) -> &Path {
        self.catalog.directory().path()
    }

    /// Refreshes explicit files and the catalog using metadata, without decoding files.
    ///
    /// Identical absolute paths are listed once. Unreadable files keep their error row.
    /// Cached timestamps are shown only while file length and modification time match.
    ///
    /// # Errors
    /// Returns a directory enumeration failure for an accessible startup directory or override.
    pub fn snapshots(&self) -> Result<Vec<SnapshotInfo>, McpError> {
        let entries = match self.catalog.entries() {
            Ok(entries) => entries,
            Err(_) if self.ignore_directory_errors => Vec::new(),
            Err(source) => return Err(Self::directory_error(source)),
        };
        let mut named = BTreeMap::new();
        let mut unnamed = Vec::new();
        for entry in entries {
            let info = self.catalog_info(&entry);
            if let Some(path) = &info.path {
                named.insert(path.clone(), info);
            } else {
                unnamed.push(info);
            }
        }
        for path in &self.explicit {
            named
                .entry(path.clone())
                .or_insert_with(|| self.explicit_info(path));
        }
        let mut entries: Vec<_> = named.into_values().collect();
        entries.append(&mut unnamed);
        Ok(entries)
    }

    /// Resolves a file path, an ASCII volume name (`C:` or `c`), or the sole discovered file.
    ///
    /// # Errors
    /// Returns `no_snapshot`, `ambiguous_snapshot`, `unsupported_source`, or discovery errors.
    pub fn select(&self, snapshot: Option<&str>) -> Result<PathBuf, McpError> {
        let volume = snapshot.and_then(Self::volume);
        if let Some(path) = snapshot.filter(|_| volume.is_none()) {
            let path = Self::absolute(Path::new(path))?;
            SnapshotCache::check_extension(&path)?;
            return Ok(path);
        }
        let candidates: Vec<_> = self
            .snapshots()?
            .into_iter()
            .filter_map(|entry| entry.path)
            .filter(|path| {
                volume.is_none_or(|letter| {
                    SnapshotCache::check_extension(path).is_ok()
                        && path
                            .file_stem()
                            .is_some_and(|stem| stem.eq_ignore_ascii_case(letter.to_string()))
                })
            })
            .collect();
        match candidates.as_slice() {
            [] => Err(McpError::NoSnapshot {
                volume: volume.unwrap_or('C'),
                directory: self.directory().to_owned(),
            }),
            [path] => {
                SnapshotCache::check_extension(path)?;
                Ok(path.clone())
            }
            _ => Err(McpError::AmbiguousSnapshot { candidates }),
        }
    }

    /// Selects and loads a saved snapshot through the shared blocking cache.
    ///
    /// # Errors
    /// Returns selection, permission, I/O or snapshot decoding failures.
    pub fn load(&self, snapshot: Option<&str>) -> Result<Arc<ScanOutput>, McpError> {
        self.cache.load(&self.select(snapshot)?)
    }

    fn volume(value: &str) -> Option<char> {
        let mut chars = value.chars();
        match (chars.next(), chars.next(), chars.next()) {
            (Some(letter), None | Some(':'), None) if letter.is_ascii_alphabetic() => {
                Some(letter.to_ascii_uppercase())
            }
            _ => None,
        }
    }

    fn absolute(path: &Path) -> Result<PathBuf, McpError> {
        std::path::absolute(path).map_err(|source| McpError::io(path, source))
    }

    fn directory_error(error: SnapshotDirError) -> McpError {
        match error {
            SnapshotDirError::Io { path, source, .. } => McpError::io(path, source),
            source => McpError::io(PathBuf::new(), io::Error::other(source)),
        }
    }

    fn catalog_info(&self, entry: &SnapshotEntry) -> SnapshotInfo {
        let path = entry.path().map(Path::to_owned);
        let mut info = SnapshotInfo {
            name: path
                .as_deref()
                .and_then(Path::file_name)
                .map(ToOwned::to_owned),
            path,
            byte_len: entry.byte_len(),
            modified: entry.modified(),
            scan_time: ScanTime(None),
            error: entry.error().map(|source| {
                McpError::io(
                    entry.path().unwrap_or(Path::new("")),
                    io::Error::new(source.kind(), source.to_string()),
                )
            }),
        };
        self.enrich(&mut info);
        info
    }

    fn explicit_info(&self, path: &Path) -> SnapshotInfo {
        let metadata =
            fs::metadata(path).and_then(|metadata| Ok((metadata.len(), metadata.modified()?)));
        let mut info = SnapshotInfo {
            name: path.file_name().map(ToOwned::to_owned),
            path: Some(path.to_owned()),
            byte_len: metadata.as_ref().ok().map(|(length, _)| *length),
            modified: metadata.as_ref().ok().map(|(_, modified)| *modified),
            scan_time: ScanTime(None),
            error: metadata.err().map(|source| McpError::io(path, source)),
        };
        self.enrich(&mut info);
        info
    }

    fn enrich(&self, info: &mut SnapshotInfo) {
        if let (Some(path), Some(byte_len), Some(modified)) =
            (&info.path, info.byte_len, info.modified)
        {
            match self
                .cache
                .scan_time(path, Fingerprint { byte_len, modified })
            {
                Ok(time) => info.scan_time = time,
                Err(error) => info.error = Some(error),
            }
        }
    }
}
