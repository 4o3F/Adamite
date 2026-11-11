use crate::{
    cli::SourceArgs,
    error::CliError,
    render::{json::Json, text::Text},
    report::InfoReport,
    source::Source,
};
use adamite_core::scan::ScanOutput;
use std::io::Write;

pub(crate) struct Info(pub(crate) SourceArgs);

impl Info {
    pub(crate) fn run(&self, stdout: &mut impl Write) -> Result<(), CliError> {
        let output = Source::open(&self.0.source)?.load()?;
        self.render("info", &output, stdout)
    }

    pub(crate) fn render(
        &self,
        command: &'static str,
        output: &ScanOutput,
        stdout: &mut impl Write,
    ) -> Result<(), CliError> {
        let report = InfoReport::new(command, &self.0.source, output)?;
        if self.0.json {
            Json(&report).write(stdout)
        } else {
            Text {
                report: &report,
                bytes: self.0.bytes,
            }
            .write(stdout)
        }
    }
}
