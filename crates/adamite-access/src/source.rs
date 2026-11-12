use crate::SourceError;
use adamite_core::scan::{ScanContext, ScanOutput};
use adamite_io::{
    driver::{DriverProgress, SyncDriver},
    error::VolumeError,
    limits::ScanLimits,
    reader::{ImageReader, VolumeReader},
    snapshot::{SnapshotIoError, SnapshotReader},
};
use std::{
    fs::File,
    io::{self, Read, Seek},
    path::{Path, PathBuf},
};

/// An opened snapshot file, NTFS image path, or Windows drive source.
#[derive(Debug)]
pub enum Source {
    /// A raw Windows volume, opened when scanning starts.
    Drive {
        /// ASCII drive letter.
        letter: char,
        /// Original drive argument.
        path: PathBuf,
    },
    /// A snapshot with its file positioned at the header.
    Snapshot {
        /// Snapshot file path.
        path: PathBuf,
        /// File retained from source detection.
        file: File,
    },
    /// An NTFS image, opened when scanning starts.
    Image {
        /// Image file path.
        path: PathBuf,
    },
}

impl Source {
    /// Detects a drive, snapshot or image without scanning or decoding it.
    ///
    /// An ASCII drive letter followed by `:` selects a drive. Files with an `.adm` extension (ignoring ASCII case) or ADM magic select snapshots, including damaged headers selected by extension. Other files select NTFS images.
    ///
    /// # Errors
    /// Returns an I/O error if the file cannot be opened, read or rewound.
    pub fn open(path: &Path) -> Result<Self, SourceError> {
        let _entered = tracing::info_span!("source_detect").entered();
        if let Some(text) = path.to_str() {
            let mut chars = text.chars();
            if let (Some(letter), Some(':'), None) = (chars.next(), chars.next(), chars.next())
                && letter.is_ascii_alphabetic()
            {
                return Ok(Self::Drive {
                    letter,
                    path: path.to_owned(),
                });
            }
        }
        let mut file = File::open(path).map_err(|source| SourceError::Io {
            operation: "open source",
            path: path.to_owned(),
            source,
        })?;
        // The extension also selects damaged or truncated snapshot headers.
        let extension = path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("adm"));
        let mut magic = [0; ScanOutput::ADM_MAGIC.len()];
        let complete = match file.read_exact(&mut magic) {
            Ok(()) => true,
            Err(source) if source.kind() == io::ErrorKind::UnexpectedEof => false,
            Err(source) => {
                return Err(SourceError::Io {
                    operation: "read source header",
                    path: path.to_owned(),
                    source,
                });
            }
        };
        if extension || (complete && magic == ScanOutput::ADM_MAGIC) {
            file.rewind().map_err(|source| SourceError::Io {
                operation: "rewind snapshot",
                path: path.to_owned(),
                source,
            })?;
            Ok(Self::Snapshot {
                path: path.to_owned(),
                file,
            })
        } else {
            Ok(Self::Image {
                path: path.to_owned(),
            })
        }
    }

    /// Opens a snapshot selected by its `.adm` extension or ADM header, without scanning.
    ///
    /// # Errors
    /// Returns an I/O error when detection fails, or [`SourceError::UnsupportedSource`] for an image or raw drive. Snapshot decoding happens in [`Self::load_observed`].
    pub fn open_snapshot(path: &Path) -> Result<Self, SourceError> {
        let source = Self::open(path)?;
        if source.is_snapshot() {
            Ok(source)
        } else {
            Err(SourceError::UnsupportedSource)
        }
    }

    /// Returns whether loading this source decodes a saved snapshot.
    pub fn is_snapshot(&self) -> bool {
        matches!(self, Self::Snapshot { .. })
    }

    /// Returns the original source path.
    pub fn path(&self) -> &Path {
        match self {
            Self::Drive { path, .. } | Self::Image { path } | Self::Snapshot { path, .. } => path,
        }
    }

    /// Decodes a snapshot or scans an image or drive, sending scan progress to `observer`.
    ///
    /// Snapshots do not invoke the observer. The supplied limits apply to scans.
    ///
    /// # Errors
    /// Returns snapshot I/O or decoding errors, scan errors, or a platform error when a raw drive is requested outside Windows.
    pub fn load_observed(
        self,
        limits: &ScanLimits,
        observer: impl FnMut(DriverProgress),
    ) -> Result<ScanOutput, SourceError> {
        match self {
            Self::Snapshot { path, mut file } => {
                let _entered = tracing::info_span!("snapshot_load").entered();
                let len = file
                    .metadata()
                    .map_err(|source| SourceError::Io {
                        operation: "read snapshot length",
                        path: path.clone(),
                        source,
                    })?
                    .len();
                usize::try_from(len)
                    .map_err(|_| SourceError::SnapshotLength { path: path.clone() })?;
                SnapshotReader::read(&mut file).map_err(|error| match error {
                    SnapshotIoError::Persist { source } => SourceError::Persist {
                        operation: "decode",
                        path: path.clone(),
                        source,
                    },
                    SnapshotIoError::Io { source } => SourceError::Io {
                        operation: "read snapshot",
                        path: path.clone(),
                        source,
                    },
                    _ => SourceError::SnapshotLength { path },
                })
            }
            Self::Image { path } => Self::run_scan(limits, observer, || {
                let reader = ImageReader::open(&path)?;
                let context = reader.context()?;
                Ok((reader, context))
            })
            .map_err(|source| SourceError::Volume {
                path,
                drive: false,
                source,
            }),
            Self::Drive { letter, path } => Self::scan_drive(letter, path, limits, observer),
        }
    }

    fn run_scan<R: VolumeReader>(
        limits: &ScanLimits,
        observer: impl FnMut(DriverProgress),
        open: impl FnOnce() -> Result<(R, ScanContext), VolumeError>,
    ) -> Result<ScanOutput, VolumeError> {
        let _entered = tracing::info_span!("scan").entered();
        let (mut reader, context) = open()?;
        SyncDriver::with_limits(*limits, context)?.run_observed(&mut reader, observer)
    }

    #[cfg(windows)]
    fn scan_drive(
        letter: char,
        path: PathBuf,
        limits: &ScanLimits,
        observer: impl FnMut(DriverProgress),
    ) -> Result<ScanOutput, SourceError> {
        use adamite_io::reader::RawVolumeReader;
        Self::run_scan(limits, observer, || {
            let reader = RawVolumeReader::open_drive(letter)?;
            let context = reader.context()?;
            Ok((reader, context))
        })
        .map_err(|source| SourceError::Volume {
            path,
            drive: true,
            source,
        })
    }

    #[cfg(not(windows))]
    fn scan_drive(
        _letter: char,
        _path: PathBuf,
        _limits: &ScanLimits,
        _observer: impl FnMut(DriverProgress),
    ) -> Result<ScanOutput, SourceError> {
        Err(SourceError::RawVolumesRequireWindows)
    }
}
