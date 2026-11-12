use crate::{
    cli::TopArgs,
    error::{CliError, DirectoryLookup},
    render::Renderer,
    report::TopReport,
    source::Source,
};
use adamite_core::snapshot::FileFilter;
use std::io::Write;

pub(crate) struct Top(pub(crate) TopArgs);

impl Top {
    pub(crate) fn run(&self, stdout: &mut impl Write) -> Result<(), CliError> {
        let args = &self.0;
        let output = Source::open(&args.source.source)?.load(&args.source.limits)?;
        let report = if args.under.is_none() && args.ext.is_empty() && args.kind.is_none() {
            TopReport::new(&args.source.source, &output, args.n)?
        } else {
            let under = output
                .snapshot
                .resolve_directory(args.under.as_deref().unwrap_or("/"))?;
            let filter = match args.kind {
                Some(kind) => FileFilter::Kind(kind),
                None if !args.ext.is_empty() => FileFilter::extensions(&args.ext)?,
                None => FileFilter::All,
            };
            let n = usize::try_from(args.n).map_err(|_| CliError::Usage {
                message: "--n exceeds the supported range",
            })?;
            let ids = output.snapshot.rank_files(under, &filter, n)?;
            TopReport::from_ids(&args.source.source, &output, ids)?
        };
        Renderer::from(&args.source).write(&report, stdout)
    }
}
