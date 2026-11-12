use crate::{
    cli::{ExtArgs, GroupBy},
    error::CliError,
    render::Renderer,
    source::SourceExt,
};
use adamite_access::report::{ExtReport, KindReport};
use adamite_access::{BreakdownBy, BreakdownStats, Source};
use std::io::Write;

pub(crate) struct Ext(pub(crate) ExtArgs);

impl Ext {
    pub(crate) fn run(&self, stdout: &mut impl Write) -> Result<(), CliError> {
        let args = &self.0;
        let output = Source::open(&args.source.source)?.load(&args.source.limits)?;
        let renderer = Renderer::from(&args.source);
        let by = match args.by {
            GroupBy::Extension => BreakdownBy::Extension,
            GroupBy::Kind => BreakdownBy::Kind,
        };
        match BreakdownStats::new(&output.snapshot, args.under.as_deref(), by)? {
            BreakdownStats::Extensions(stats) => {
                let report = ExtReport::from_stats(&args.source.source, &output, &stats, args.n);
                renderer.write(&report, stdout)
            }
            BreakdownStats::Kinds(stats) => {
                let report = KindReport::new(&args.source.source, &output, &stats, args.n);
                renderer.write(&report, stdout)
            }
        }
    }
}
