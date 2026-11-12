use crate::{
    cli::{ExtArgs, GroupBy},
    error::{CliError, DirectoryLookup},
    render::Renderer,
    report::{ExtReport, KindReport},
    source::Source,
};
use std::io::Write;

pub(crate) struct Ext(pub(crate) ExtArgs);

impl Ext {
    pub(crate) fn run(&self, stdout: &mut impl Write) -> Result<(), CliError> {
        let args = &self.0;
        let output = Source::open(&args.source.source)?.load(&args.source.limits)?;
        let renderer = Renderer::from(&args.source);
        if matches!(args.by, GroupBy::Extension) && args.under.is_none() {
            let report = ExtReport::new(&args.source.source, &output, args.n);
            return renderer.write(&report, stdout);
        }
        let under = output
            .snapshot
            .resolve_directory(args.under.as_deref().unwrap_or("/"))?;
        match args.by {
            GroupBy::Extension => {
                let stats = output.snapshot.extension_stats_under(under)?;
                let report = ExtReport::from_stats(&args.source.source, &output, &stats, args.n);
                renderer.write(&report, stdout)
            }
            GroupBy::Kind => {
                let stats = output.snapshot.kind_stats(under)?;
                let report = KindReport::new(&args.source.source, &output, &stats, args.n);
                renderer.write(&report, stdout)
            }
        }
    }
}
