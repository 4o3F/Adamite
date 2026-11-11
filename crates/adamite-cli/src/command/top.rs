use crate::{
    cli::TopArgs,
    error::CliError,
    render::{json::Json, text::TopText},
    report::TopReport,
    source::Source,
};
use std::io::Write;

pub(crate) struct Top(pub(crate) TopArgs);

impl Top {
    pub(crate) fn run(&self, stdout: &mut impl Write) -> Result<(), CliError> {
        let args = &self.0;
        let output = Source::open(&args.source.source)?.load()?;
        let report = TopReport::new(&args.source.source, &output, args.n)?;
        if args.source.json {
            Json(&report).write(stdout)
        } else {
            TopText {
                report: &report,
                bytes: args.source.bytes,
            }
            .write(stdout)
        }
    }
}
