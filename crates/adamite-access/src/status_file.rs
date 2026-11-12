use crate::ScanJobError;
use adamite_io::{
    driver::DriverProgress,
    status::{ScanStatus, ScanStatusWriter, StatusError},
};
use std::{io, path::Path, time::Instant};

/// Writes scan status while retaining the first progress error for the caller.
#[derive(Debug)]
pub struct StatusFile {
    writer: ScanStatusWriter,
    error: Option<StatusError>,
}

impl StatusFile {
    /// Exclusively creates a status file, leaving existing files untouched.
    ///
    /// # Errors
    /// Returns an existing-status-file error or a creation I/O error.
    pub fn create(path: &Path) -> Result<Self, ScanJobError> {
        let writer = ScanStatusWriter::create(path).map_err(|source| {
            if source.kind() == io::ErrorKind::AlreadyExists {
                ScanJobError::StatusExists {
                    path: path.to_owned(),
                }
            } else {
                ScanJobError::Io {
                    operation: "create status file",
                    path: path.to_owned(),
                    source,
                }
            }
        })?;
        Ok(Self {
            writer,
            error: None,
        })
    }

    /// Reports progress with the writer's phase and time throttling, retaining failures.
    pub fn progress(&mut self, progress: &DriverProgress) {
        if self.error.is_none() {
            self.error = self.writer.progress(progress, Instant::now()).err();
        }
    }

    /// Takes the first retained progress-writing error, if any.
    ///
    /// # Errors
    /// Returns the retained status error and clears it.
    pub fn check(&mut self) -> Result<(), ScanJobError> {
        match self.error.take() {
            Some(source) => Err(ScanJobError::Status { source }),
            None => Ok(()),
        }
    }

    /// Writes the caller's final saved or failed outcome and flushes it.
    ///
    /// # Errors
    /// Returns a status serialization or file I/O error.
    pub fn finish(&mut self, status: &ScanStatus) -> Result<(), ScanJobError> {
        self.writer
            .write(status)
            .map_err(|source| ScanJobError::Status { source })
    }
}
