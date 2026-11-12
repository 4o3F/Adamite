use crate::{cli::SearchArgs, error::CliError, render::Renderer, source::SourceExt};
use adamite_access::report::SearchReport;
use adamite_access::{DirectoryLookup, Source};
use adamite_core::snapshot::NamePattern;
use std::io::Write;

pub(crate) struct Search(pub(crate) SearchArgs);

impl Search {
    pub(crate) fn run(&self, stdout: &mut impl Write) -> Result<(), CliError> {
        let args = &self.0;
        let pattern = NamePattern::glob(&args.pattern)?;
        let output = Source::open(&args.source.source)?.load(&args.source.limits)?;
        let under = output
            .snapshot
            .resolve_directory(args.under.as_deref().unwrap_or("/"))?;
        let page = output
            .snapshot
            .search(under, &pattern, args.offset, usize::from(args.limit))?;
        let report = SearchReport::new(&args.source.source, &output, &page, args.offset)?;
        Renderer::from(&args.source).write(&report, stdout)
    }
}
