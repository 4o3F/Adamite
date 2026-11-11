use crate::{
    cli::ExtArgs,
    error::CliError,
    render::{json::Json, text::ExtText},
    report::ExtReport,
    source::Source,
};
use std::io::Write;

pub(crate) struct Ext(pub(crate) ExtArgs);

impl Ext {
    pub(crate) fn run(&self, stdout: &mut impl Write) -> Result<(), CliError> {
        let args = &self.0;
        let output = Source::open(&args.source.source)?.load()?;
        let report = ExtReport::new(&args.source.source, &output, args.n);
        if args.source.json {
            Json(&report).write(stdout)
        } else {
            ExtText {
                report: &report,
                bytes: args.source.bytes,
            }
            .write(stdout)
        }
    }
}
