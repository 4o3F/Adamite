use crate::{cli::TopArgs, error::CliError, render::Renderer, source::SourceExt};
use adamite_access::Source;
use adamite_access::report::TopReport;
use adamite_core::snapshot::FileFilter;
use std::io::Write;

pub(crate) struct Top(pub(crate) TopArgs);

impl Top {
    pub(crate) fn run(&self, stdout: &mut impl Write) -> Result<(), CliError> {
        let args = &self.0;
        let output = Source::open(&args.source.source)?.load(&args.source.limits)?;
        let filter = match args.kind {
            Some(kind) => Some(FileFilter::Kind(kind)),
            None if !args.ext.is_empty() => Some(FileFilter::extensions(&args.ext)?),
            None => None,
        };
        let report = TopReport::select(
            &args.source.source,
            &output,
            args.n,
            args.under.as_deref(),
            filter.as_ref(),
        )?;
        Renderer::from(&args.source).write(&report, stdout)
    }
}
