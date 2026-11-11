use crate::error::CliError;
use adamite_core::scan::{ScanContext, ScanOutput};
use adamite_io::{
    driver::SyncDriver,
    error::VolumeError,
    limits::ScanLimits,
    reader::{ImageReader, VolumeReader},
};
use std::{
    fs::File,
    io::{self, Read, Seek, Write},
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

    pub(crate) fn load(self, limits: ScanLimits) -> Result<ScanOutput, CliError> {
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
                let len = usize::try_from(len)
                    .map_err(|_| CliError::SnapshotLength { path: path.clone() })?;
                let mut bytes = Vec::new();
                bytes
                    .try_reserve_exact(len)
                    .map_err(|source| CliError::Allocation {
                        path: path.clone(),
                        source,
                    })?;
                bytes.resize(len, 0);
                file.read_exact(&mut bytes).map_err(|source| CliError::Io {
                    operation: "read snapshot",
                    path: path.clone(),
                    source,
                })?;
                ScanOutput::from_bytes(&bytes).map_err(|source| CliError::Persist {
                    operation: "decode",
                    path,
                    source,
                })
            }
            Self::Image { path } => Self::run_scan(&path, limits, || {
                let reader = ImageReader::open(&path)?;
                let context = reader.context()?;
                Ok((reader, context))
            })
            .map_err(|source| CliError::Volume {
                path,
                drive: false,
                source,
            }),
            Self::Drive { letter, path } => Self::scan_drive(letter, path, limits),
        }
    }

    fn run_scan<R: VolumeReader>(
        path: &Path,
        limits: ScanLimits,
        open: impl FnOnce() -> Result<(R, ScanContext), VolumeError>,
    ) -> Result<ScanOutput, VolumeError> {
        let _entered = tracing::info_span!("scan").entered();
        let _ = writeln!(io::stderr().lock(), "scanning {}…", path.display());
        let (mut reader, context) = open()?;
        SyncDriver::with_limits(limits, context)?.run(&mut reader)
    }

    #[cfg(windows)]
    fn scan_drive(letter: char, path: PathBuf, limits: ScanLimits) -> Result<ScanOutput, CliError> {
        use adamite_io::reader::RawVolumeReader;
        Self::run_scan(&path, limits, || {
            let reader = RawVolumeReader::open_drive(letter)?;
            let context = reader.context()?;
            Ok((reader, context))
        })
        .map_err(|source| CliError::Volume {
            path,
            drive: true,
            source,
        })
    }

    #[cfg(not(windows))]
    fn scan_drive(
        _letter: char,
        _path: PathBuf,
        _limits: ScanLimits,
    ) -> Result<ScanOutput, CliError> {
        Err(CliError::Usage {
            message: "raw volumes require Windows",
        })
    }
}
