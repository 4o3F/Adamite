use crate::{error::CliError, report::InfoReport};
use std::io::Write;

pub(crate) struct Json<'a>(pub(crate) &'a InfoReport<'a>);

impl Json<'_> {
    pub(crate) fn write(&self, stdout: &mut impl Write) -> Result<(), CliError> {
        let mut bytes = serde_json::to_vec(self.0).map_err(|source| CliError::Json { source })?;
        bytes.push(b'\n');
        stdout
            .write_all(&bytes)
            .map_err(|source| CliError::Output { source })
    }
}
