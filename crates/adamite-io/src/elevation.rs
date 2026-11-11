//! Windows administrator-process launching and command-line argument quoting.

use snafu::Snafu;
use std::{
    ffi::{OsStr, OsString},
    io,
};

#[cfg(windows)]
mod ffi;

/// Failure to validate arguments, query elevation, launch or wait for a child.
#[derive(Debug, Snafu)]
#[non_exhaustive]
pub enum ElevationError {
    /// The user cancelled the administrator prompt.
    #[snafu(display("elevation was declined"))]
    Declined,
    /// An argument contains a quote or NUL character.
    #[snafu(display("invalid elevation argument {argument:?}: {reason}"))]
    Argument {
        /// Rejected argument.
        argument: OsString,
        /// Reason the argument cannot be passed to the child.
        reason: &'static str,
    },
    /// Querying the current process token failed.
    #[snafu(display("failed to query process elevation: {source}"))]
    Query {
        /// Host token-query failure.
        source: io::Error,
    },
    /// Resolving or launching the current executable failed.
    #[snafu(display("failed to launch elevated process: {source}"))]
    Launch {
        /// Host launch failure.
        source: io::Error,
    },
    /// Waiting for the child or reading its exit code failed.
    #[snafu(display("failed to wait for elevated process: {source}"))]
    Wait {
        /// Host wait failure.
        source: io::Error,
    },
}

/// Quoted Windows parameters, preserving native argument encoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandLine(OsString);

impl CommandLine {
    /// Quotes every argument and doubles backslashes before its closing quote.
    ///
    /// # Errors
    /// Returns [`ElevationError::Argument`] for an argument containing a quote or NUL.
    pub fn new(args: impl IntoIterator<Item = impl AsRef<OsStr>>) -> Result<Self, ElevationError> {
        let mut parameters = OsString::new();
        for arg in args {
            let arg = arg.as_ref();
            let bytes = arg.as_encoded_bytes();
            let reason = if bytes.contains(&b'"') {
                Some("quotes are not supported")
            } else if bytes.contains(&0) {
                Some("NUL characters are not supported")
            } else {
                None
            };
            if let Some(reason) = reason {
                return Err(ElevationError::Argument {
                    argument: arg.to_owned(),
                    reason,
                });
            }
            if !parameters.is_empty() {
                parameters.push(" ");
            }
            parameters.push("\"");
            parameters.push(arg);
            for _ in bytes.iter().rev().take_while(|byte| **byte == b'\\') {
                parameters.push("\\");
            }
            parameters.push("\"");
        }
        Ok(Self(parameters))
    }

    /// Borrows the encoded parameters without a terminating NUL.
    #[must_use]
    pub fn as_os_str(&self) -> &OsStr {
        &self.0
    }
}

/// An elevated child process whose handle closes when dropped.
#[cfg(windows)]
#[derive(Debug)]
pub struct ElevatedProcess(ffi::ProcessHandle);

#[cfg(windows)]
impl ElevatedProcess {
    /// Queries whether the current process token is elevated.
    ///
    /// # Errors
    /// Returns [`ElevationError::Query`] if the process token cannot be read.
    pub fn is_current() -> Result<bool, ElevationError> {
        ffi::ProcessHandle::is_current().map_err(|source| ElevationError::Query { source })
    }

    /// Runs the current executable with the supplied arguments through the UAC prompt.
    ///
    /// The child uses `runas` with a hidden window and synchronous shell launch completion.
    ///
    /// # Errors
    /// Returns [`ElevationError::Argument`] for invalid arguments, [`ElevationError::Declined`]
    /// when the prompt is cancelled, or [`ElevationError::Launch`] for host failures.
    pub fn launch(
        args: impl IntoIterator<Item = impl AsRef<OsStr>>,
    ) -> Result<Self, ElevationError> {
        use windows::Win32::Foundation::ERROR_CANCELLED;

        let parameters = CommandLine::new(args)?;
        let executable =
            std::env::current_exe().map_err(|source| ElevationError::Launch { source })?;
        ffi::ProcessHandle::launch(&executable, &parameters)
            .map(Self)
            .map_err(|source| {
                if source.raw_os_error() == Some(ERROR_CANCELLED.to_hresult().0) {
                    ElevationError::Declined
                } else {
                    ElevationError::Launch { source }
                }
            })
    }

    /// Returns the exit code if the child has finished, without blocking.
    ///
    /// # Errors
    /// Returns [`ElevationError::Wait`] if waiting or reading the exit code fails.
    pub fn try_wait(&mut self) -> Result<Option<u32>, ElevationError> {
        self.0
            .try_wait()
            .map_err(|source| ElevationError::Wait { source })
    }

    /// Blocks until the child exits and returns its exit code.
    ///
    /// # Errors
    /// Returns [`ElevationError::Wait`] if waiting or reading the exit code fails.
    pub fn wait(&mut self) -> Result<u32, ElevationError> {
        self.0
            .wait()
            .map_err(|source| ElevationError::Wait { source })
    }
}
