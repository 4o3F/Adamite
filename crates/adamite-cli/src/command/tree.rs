use crate::{cli::TreeArgs, error::CliError, render::Renderer, source::SourceExt};
use adamite_access::{Source, report::TreeReport};
use std::io::Write;

pub(crate) struct Tree(pub(crate) TreeArgs);

impl Tree {
    pub(crate) fn run(&self, stdout: &mut impl Write) -> Result<(), CliError> {
        let args = &self.0;
        let output = Source::open(&args.source.source)?.load(&args.source.limits)?;
        let report = TreeReport::new(
            &args.source.source,
            &output,
            &args.path,
            usize::from(args.budget),
            args.min_share,
        )?;
        Renderer::from(&args.source).write(&report, stdout)
    }
}
