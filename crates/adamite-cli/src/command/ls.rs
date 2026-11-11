use crate::{cli::LsArgs, error::CliError, render::Renderer, report::LsReport, source::Source};
use std::io::Write;

pub(crate) struct Ls(pub(crate) LsArgs);

impl Ls {
    pub(crate) fn run(&self, stdout: &mut impl Write) -> Result<(), CliError> {
        let args = &self.0;
        let output = Source::open(&args.source.source)?.load(args.source.limits.limits())?;
        let report = LsReport::new(
            &args.source.source,
            &output,
            &args.path,
            args.offset,
            args.limit,
        )?;
        Renderer::from(&args.source).write(&report, stdout)
    }
}
