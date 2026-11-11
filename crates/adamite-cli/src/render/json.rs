use crate::error::CliError;
use serde::Serialize;
use std::io::Write;

pub(crate) struct Json<'a, T>(pub(crate) &'a T);

impl<T: Serialize> Json<'_, T> {
    pub(crate) fn write(&self, stdout: &mut impl Write) -> Result<(), CliError> {
        let mut bytes = serde_json::to_vec(self.0).map_err(|source| CliError::Json { source })?;
        bytes.push(b'\n');
        stdout
            .write_all(&bytes)
            .map_err(|source| CliError::Output { source })
    }
}
