use crate::{ScanJobError, SourceError};
use snafu::Snafu;
use std::error::Error;

/// A scan job's complete human-readable failure and optional child exit code.
#[derive(Debug, Clone, PartialEq, Eq, Snafu)]
#[snafu(module, display("{message}"))]
pub struct JobError {
    /// Message including any underlying causes.
    pub message: String,
    /// Reported child exit code, preserving both signed status codes and Windows process codes.
    pub exit_code: Option<i64>,
    elevation_declined: bool,
}

impl JobError {
    /// Creates a failure without a child exit code.
    pub(super) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            exit_code: None,
            elevation_declined: false,
        }
    }

    /// Reports whether the user declined the administrator-access prompt.
    #[must_use]
    pub const fn is_elevation_declined(&self) -> bool {
        self.elevation_declined
    }

    /// Formats an error and its causes, retaining the supplied child exit code when available.
    pub fn from_error(error: &dyn Error, exit_code: Option<i64>) -> Self {
        let mut message = error.to_string();
        let mut source = error.source();
        while let Some(cause) = source {
            message.push_str("\ncaused by: ");
            message.push_str(&cause.to_string());
            source = cause.source();
        }
        Self {
            message,
            exit_code,
            elevation_declined: false,
        }
    }
}

impl From<SourceError> for JobError {
    fn from(error: SourceError) -> Self {
        Self::from_error(&error, None)
    }
}

impl From<ScanJobError> for JobError {
    fn from(error: ScanJobError) -> Self {
        let exit_code = match &error {
            ScanJobError::ChildFailed { exit_code, .. } => Some(i64::from(*exit_code)),
            ScanJobError::ChildExited { exit_code } => Some(i64::from(*exit_code)),
            _ => None,
        };
        let mut result = Self::from_error(&error, exit_code);
        result.elevation_declined = matches!(error, ScanJobError::ElevationDeclined);
        result
    }
}
