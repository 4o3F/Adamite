pub(crate) mod json;
pub(crate) mod text;

use crate::{cli::SourceArgs, error::CliError};
use serde::Serialize;
use std::io::{self, Write};

pub(crate) trait Report: Serialize {
    fn write_text(&self, stdout: &mut impl Write, raw_bytes: bool) -> io::Result<()>;
}

pub(crate) struct Renderer {
    json: bool,
    raw_bytes: bool,
}

impl From<&SourceArgs> for Renderer {
    fn from(args: &SourceArgs) -> Self {
        Self {
            json: args.json,
            raw_bytes: args.bytes,
        }
    }
}

impl Renderer {
    pub(crate) fn write(
        &self,
        report: &impl Report,
        stdout: &mut impl Write,
    ) -> Result<(), CliError> {
        if self.json {
            json::Json(report).write(stdout)
        } else {
            report
                .write_text(stdout, self.raw_bytes)
                .map_err(|source| CliError::Output { source })
        }
    }
}
