use crate::{
    cli::LimitArgs, elevated_scan::ElevatedScan, error::CliError, save::PublishMode,
    scan_progress_view::ScanProgressView, status_file::StatusFile,
};
use adamite_core::scan::{ScanContext, ScanOutput};
use adamite_io::{
    driver::{DriverProgress, SyncDriver},
    error::VolumeError,
    reader::{ImageReader, VolumeReader},
    snapshot::{SnapshotIoError, SnapshotReader},
};
use std::{
    fs::File,
    io::{self, Read, Seek},
    path::{Path, PathBuf},
};

#[derive(Debug)]
pub(crate) enum Source {
    Drive { letter: char, path: PathBuf },
    Snapshot { path: PathBuf, file: File },
    Image { path: PathBuf },
}

impl Source {
    pub(crate) fn open(path: &Path) -> Result<Self, CliError> {
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
        let mut file = File::open(path).map_err(|source| CliError::Io {
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
                return Err(CliError::Io {
                    operation: "read source header",
                    path: path.to_owned(),
                    source,
                });
            }
        };
        if extension || (complete && magic == ScanOutput::ADM_MAGIC) {
            file.rewind().map_err(|source| CliError::Io {
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

    pub(crate) fn is_snapshot(&self) -> bool {
        matches!(self, Self::Snapshot { .. })
    }

    pub(crate) fn elevation(
        &self,
        limits: &LimitArgs,
        destination: Option<(&Path, PublishMode)>,
    ) -> Result<Option<ElevatedScan>, CliError> {
        if matches!(self, Self::Drive { .. }) && ElevatedScan::required(limits)? {
            ElevatedScan::new(destination).map(Some)
        } else {
            Ok(None)
        }
    }

    pub(crate) fn load(self, limits: &LimitArgs) -> Result<ScanOutput, CliError> {
        if let Some(elevated) = self.elevation(limits, None)?
            && let Self::Drive { path, .. } = &self
        {
            return elevated.run(path, limits, || ScanProgressView::observer(path, None));
        }
        self.load_cli(limits, None)
    }

    pub(crate) fn load_cli(
        self,
        limits: &LimitArgs,
        status: Option<&mut StatusFile>,
    ) -> Result<ScanOutput, CliError> {
        if self.is_snapshot() || (!cfg!(windows) && matches!(self, Self::Drive { .. })) {
            return self.load_in_process(limits, |_| {});
        }
        let observer = ScanProgressView::observer(self.path(), status);
        self.load_in_process(limits, observer)
    }

    fn path(&self) -> &Path {
        match self {
            Self::Drive { path, .. } | Self::Image { path } | Self::Snapshot { path, .. } => path,
        }
    }

    /// Loads or scans without creating a terminal progress view.
    pub(crate) fn load_in_process(
        self,
        limits: &LimitArgs,
        observer: impl FnMut(DriverProgress),
    ) -> Result<ScanOutput, CliError> {
        match self {
            Self::Snapshot { path, mut file } => {
                let _entered = tracing::info_span!("snapshot_load").entered();
                let len = file
                    .metadata()
                    .map_err(|source| CliError::Io {
                        operation: "read snapshot length",
                        path: path.clone(),
                        source,
                    })?
                    .len();
                usize::try_from(len)
                    .map_err(|_| CliError::SnapshotLength { path: path.clone() })?;
                SnapshotReader::read(&mut file).map_err(|error| match error {
                    SnapshotIoError::Persist { source } => CliError::Persist {
                        operation: "decode",
                        path: path.clone(),
                        source,
                    },
                    SnapshotIoError::Io { source } => CliError::Io {
                        operation: "read snapshot",
                        path: path.clone(),
                        source,
                    },
                    _ => CliError::SnapshotLength { path },
                })
            }
            Self::Image { path } => Self::run_scan(limits, observer, || {
                let reader = ImageReader::open(&path)?;
                let context = reader.context()?;
                Ok((reader, context))
            })
            .map_err(|source| CliError::Volume {
                path,
                drive: false,
                no_elevate: limits.no_elevate,
                source,
            }),
            Self::Drive { letter, path } => Self::scan_drive(letter, path, limits, observer),
        }
    }

    fn run_scan<R: VolumeReader>(
        limits: &LimitArgs,
        observer: impl FnMut(DriverProgress),
        open: impl FnOnce() -> Result<(R, ScanContext), VolumeError>,
    ) -> Result<ScanOutput, VolumeError> {
        let _entered = tracing::info_span!("scan").entered();
        let (mut reader, context) = open()?;
        SyncDriver::with_limits(limits.limits(), context)?.run_observed(&mut reader, observer)
    }

    #[cfg(windows)]
    fn scan_drive(
        letter: char,
        path: PathBuf,
        limits: &LimitArgs,
        observer: impl FnMut(DriverProgress),
    ) -> Result<ScanOutput, CliError> {
        use adamite_io::reader::RawVolumeReader;
        Self::run_scan(limits, observer, || {
            let reader = RawVolumeReader::open_drive(letter)?;
            let context = reader.context()?;
            Ok((reader, context))
        })
        .map_err(|source| CliError::Volume {
            path,
            drive: true,
            no_elevate: limits.no_elevate,
            source,
        })
    }

    #[cfg(not(windows))]
    fn scan_drive(
        _letter: char,
        _path: PathBuf,
        _limits: &LimitArgs,
        _observer: impl FnMut(DriverProgress),
    ) -> Result<ScanOutput, CliError> {
        Err(CliError::Usage {
            message: "raw volumes require Windows",
        })
    }
}
