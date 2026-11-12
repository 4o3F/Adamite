use crate::ReportError;
use serde::Serialize;
use std::io::Write;

/// A borrowed serializable value written as one JSON line.
#[derive(Debug)]
pub struct Json<'a, T>(
    /// Value to serialize.
    pub &'a T,
);

impl<T: Serialize> Json<'_, T> {
    /// Serializes the value and appends a newline before writing output.
    ///
    /// # Errors
    /// Returns a serialization error or an error from the supplied writer.
    pub fn write(&self, stdout: &mut impl Write) -> Result<(), ReportError> {
        let mut bytes =
            serde_json::to_vec(self.0).map_err(|source| ReportError::Json { source })?;
        bytes.push(b'\n');
        stdout
            .write_all(&bytes)
            .map_err(|source| ReportError::Output { source })
    }
}
