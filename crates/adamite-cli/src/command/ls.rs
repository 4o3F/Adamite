use crate::{
    cli::LsArgs,
    error::CliError,
    render::{json::Json, text::LsText},
    report::LsReport,
    source::Source,
};
use std::io::Write;

pub(crate) struct Ls(pub(crate) LsArgs);

impl Ls {
    pub(crate) fn run(&self, stdout: &mut impl Write) -> Result<(), CliError> {
        let args = &self.0;
        let output = Source::open(&args.source.source)?.load()?;
        let report = LsReport::new(
            &args.source.source,
            &output,
            &args.path,
            args.offset,
            args.limit,
        )?;
        if args.source.json {
            Json(&report).write(stdout)
        } else {
            LsText {
                report: &report,
                bytes: args.source.bytes,
            }
            .write(stdout)
        }
    }
}
