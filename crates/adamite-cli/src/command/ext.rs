use crate::{cli::ExtArgs, error::CliError, render::Renderer, report::ExtReport, source::Source};
use std::io::Write;

pub(crate) struct Ext(pub(crate) ExtArgs);

impl Ext {
    pub(crate) fn run(&self, stdout: &mut impl Write) -> Result<(), CliError> {
        let args = &self.0;
        let output = Source::open(&args.source.source)?.load(&args.source.limits)?;
        let report = ExtReport::new(&args.source.source, &output, args.n);
        Renderer::from(&args.source).write(&report, stdout)
    }
}
