use crate::{cli::SourceArgs, command::info::Info, error::CliError, source::Source};
use std::io::Write;

pub(crate) struct Scan(pub(crate) SourceArgs);

impl Scan {
    pub(crate) fn run(self, stdout: &mut impl Write) -> Result<(), CliError> {
        let source = Source::open(&self.0.source)?;
        if source.is_snapshot() {
            return Err(CliError::Usage {
                message: "source is already a snapshot",
            });
        }
        let output = source.load()?;
        Info(self.0).render("scan", &output, stdout)
    }
}
