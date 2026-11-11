use crate::error::CliError;
use adamite_io::{
    driver::DriverProgress,
    status::{ScanStatus, ScanStatusWriter, StatusError},
};
use std::{io, path::Path, time::Instant};

#[derive(Debug)]
pub(crate) struct StatusFile {
    writer: ScanStatusWriter,
    error: Option<StatusError>,
}

impl StatusFile {
    pub(crate) fn create(path: &Path) -> Result<Self, CliError> {
        let writer = ScanStatusWriter::create(path).map_err(|source| {
            if source.kind() == io::ErrorKind::AlreadyExists {
                CliError::StatusExists {
                    path: path.to_owned(),
                }
            } else {
                CliError::Io {
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

    pub(crate) fn progress(&mut self, progress: &DriverProgress) {
        if self.error.is_none() {
            self.error = self.writer.progress(progress, Instant::now()).err();
        }
    }

    pub(crate) fn check(&mut self) -> Result<(), CliError> {
        match self.error.take() {
            Some(source) => Err(CliError::Status { source }),
            None => Ok(()),
        }
    }

    pub(crate) fn finish(
        &mut self,
        result: &Result<Option<ScanStatus>, CliError>,
    ) -> Result<(), CliError> {
        let failure;
        let status = match result {
            Ok(Some(status)) => status,
            Ok(None) => return Ok(()),
            Err(error) => {
                failure = ScanStatus::Failed {
                    message: error.message(),
                    exit_code: i32::from(error.exit_code()),
                };
                &failure
            }
        };
        self.writer
            .write(status)
            .map_err(|source| CliError::Status { source })
    }
}
