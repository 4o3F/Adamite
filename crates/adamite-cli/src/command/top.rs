use crate::{cli::TopArgs, error::CliError, render::Renderer, report::TopReport, source::Source};
use std::io::Write;

pub(crate) struct Top(pub(crate) TopArgs);

impl Top {
    pub(crate) fn run(&self, stdout: &mut impl Write) -> Result<(), CliError> {
        let args = &self.0;
        let output = Source::open(&args.source.source)?.load()?;
        let report = TopReport::new(&args.source.source, &output, args.n)?;
        Renderer::from(&args.source).write(&report, stdout)
    }
}
