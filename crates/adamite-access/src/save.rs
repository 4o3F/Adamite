use crate::{ScanJobError, Source};
use adamite_core::scan::ScanOutput;
use adamite_format::Escaped;
use adamite_io::snapshot::{SnapshotIoError, SnapshotWriter};
use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::PathBuf,
    process,
};

/// Number of retries after a temporary filename collision.
const TEMP_RETRIES: u8 = 16;

/// Whether publication must preserve an existing destination or replace it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishMode {
    /// Refuse an existing destination, including one created during the scan.
    NoClobber,
    /// Replace the destination with the completed temporary snapshot.
    Replace,
}

/// Owns the destination and publishes a fully written snapshot beside its temporary file.
#[derive(Debug)]
pub struct SnapshotFile {
    path: PathBuf,
    mode: PublishMode,
}

impl SnapshotFile {
    /// Selects a destination and publication mode without creating a file.
    pub fn new(path: PathBuf, mode: PublishMode) -> Self {
        Self { path, mode }
    }

    /// Reads the published snapshot length in bytes.
    ///
    /// # Errors
    /// Returns an I/O error or a length that exceeds the platform's address space.
    pub fn length(&self) -> Result<usize, ScanJobError> {
        let bytes = fs::metadata(&self.path)
            .map_err(|source| self.io_error("read saved snapshot length", source))?
            .len();
        usize::try_from(bytes).map_err(|_| ScanJobError::SnapshotLength {
            path: self.path.clone(),
        })
    }

    /// Rejects conflicting destinations before scanning an image or drive.
    ///
    /// # Errors
    /// Returns destination inspection errors, existing outputs in no-clobber mode, directories or replacement of the source image.
    pub fn preflight(&self, source: &Source) -> Result<(), ScanJobError> {
        if fs::metadata(&self.path).is_ok_and(|metadata| metadata.is_dir()) {
            return Err(ScanJobError::OutputIsDirectory {
                path: self.path.clone(),
            });
        }
        if matches!(self.mode, PublishMode::NoClobber) {
            return match fs::symlink_metadata(&self.path) {
                Ok(_) => Err(ScanJobError::OutputExists {
                    path: self.path.clone(),
                }),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(source) => Err(self.io_error("inspect output", source)),
            };
        }
        let Source::Image { path } = source else {
            return Ok(());
        };
        let destination = match fs::canonicalize(&self.path) {
            Ok(path) => path,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(source) => return Err(self.io_error("resolve output", source)),
        };
        let image = fs::canonicalize(path).map_err(|source| ScanJobError::Io {
            operation: "resolve source image",
            path: path.clone(),
            source,
        })?;
        if image == destination {
            return Err(ScanJobError::SameAsSource {
                path: self.path.clone(),
            });
        }
        Ok(())
    }

    /// Encodes, writes, synchronizes and closes the temporary file before publishing it.
    ///
    /// # Errors
    /// Returns encoding, file I/O or publication errors. Temporary files are removed on failure.
    pub fn save(&self, output: &ScanOutput) -> Result<usize, ScanJobError> {
        let _entered = tracing::info_span!("snapshot_save").entered();
        let (mut temporary, mut file) = self.create_temporary()?;
        let written = SnapshotWriter::write(output, &mut file);
        let result = written.and_then(|length| {
            file.sync_all().map_err(SnapshotIoError::from)?;
            Ok(length)
        });
        drop(file);
        let length = result.map_err(|error| match error {
            SnapshotIoError::Persist { source } => ScanJobError::Persist {
                operation: "encode",
                path: self.path.clone(),
                source,
            },
            SnapshotIoError::Io { source } => ScanJobError::Io {
                operation: "write and sync snapshot",
                path: temporary.path.clone(),
                source,
            },
            other => self.io_error("write and sync snapshot", io::Error::other(other)),
        })?;
        match self.mode {
            PublishMode::NoClobber => {
                fs::hard_link(&temporary.path, &self.path)
                    .map_err(|source| self.link_error(source))?;
                temporary.remove();
            }
            PublishMode::Replace => {
                fs::rename(&temporary.path, &self.path)
                    .map_err(|source| self.io_error("replace output", source))?;
                temporary.remove_on_drop = false;
            }
        }
        Ok(length)
    }

    fn create_temporary(&self) -> Result<(TemporaryFile, File), ScanJobError> {
        let name = self.path.file_name().ok_or_else(|| {
            self.io_error(
                "create temporary snapshot",
                io::Error::new(io::ErrorKind::InvalidInput, "output has no filename"),
            )
        })?;
        for attempt in 0..=TEMP_RETRIES {
            let mut temporary_name = OsString::from(".");
            temporary_name.push(name);
            temporary_name.push(format!(".{}-{attempt}.tmp", process::id()));
            let path = self.path.with_file_name(temporary_name);
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => {
                    return Ok((
                        TemporaryFile {
                            path,
                            remove_on_drop: true,
                        },
                        file,
                    ));
                }
                Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {}
                Err(source) => {
                    return Err(ScanJobError::Io {
                        operation: "create temporary snapshot",
                        path,
                        source,
                    });
                }
            }
        }
        Err(self.io_error(
            "create temporary snapshot",
            io::Error::from(io::ErrorKind::AlreadyExists),
        ))
    }

    fn link_error(&self, source: io::Error) -> ScanJobError {
        if source.kind() == io::ErrorKind::AlreadyExists {
            return ScanJobError::OutputExists {
                path: self.path.clone(),
            };
        }
        let unsupported = source.kind() == io::ErrorKind::Unsupported;
        // CreateHardLinkW can report ERROR_INVALID_FUNCTION or ERROR_NOT_SUPPORTED;
        // std does not classify these as Unsupported. Linux EOPNOTSUPP already is.
        #[cfg(windows)]
        let unsupported = unsupported || matches!(source.raw_os_error(), Some(1 | 50));
        if unsupported {
            ScanJobError::UnsupportedNoClobber {
                path: self.path.clone(),
                source,
            }
        } else {
            self.io_error("publish snapshot", source)
        }
    }

    fn io_error(&self, operation: &'static str, source: io::Error) -> ScanJobError {
        ScanJobError::Io {
            operation,
            path: self.path.clone(),
            source,
        }
    }
}

#[derive(Debug)]
struct TemporaryFile {
    path: PathBuf,
    remove_on_drop: bool,
}

impl TemporaryFile {
    /// Warns after publication and leaves failed cleanup armed for a retry on drop.
    fn remove(&mut self) {
        match fs::remove_file(&self.path) {
            Ok(()) => self.remove_on_drop = false,
            Err(error) => {
                let _ = writeln!(
                    io::stderr().lock(),
                    "warning: cannot remove temporary snapshot {}: {}",
                    Escaped(&self.path.to_string_lossy()),
                    Escaped(&error.to_string()),
                );
            }
        }
    }
}

impl Drop for TemporaryFile {
    fn drop(&mut self) {
        if self.remove_on_drop {
            let _ = fs::remove_file(&self.path);
        }
    }
}
