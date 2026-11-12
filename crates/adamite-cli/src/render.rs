use crate::{cli::SourceArgs, error::CliError};
use adamite_access::render::Report;
use std::io::Write;

pub(crate) struct Renderer(adamite_access::render::Renderer);

impl From<&SourceArgs> for Renderer {
    fn from(args: &SourceArgs) -> Self {
        Self(adamite_access::render::Renderer::new(args.json, args.bytes))
    }
}

impl Renderer {
    pub(crate) fn write(
        &self,
        report: &impl Report,
        stdout: &mut impl Write,
    ) -> Result<(), CliError> {
        self.0.write(report, stdout).map_err(Into::into)
    }
}
