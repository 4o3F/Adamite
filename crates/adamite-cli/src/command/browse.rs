use crate::{
    cli::BrowseArgs, error::CliError, launcher::CliSources, locate::Locator, source::Source,
};
use adamite_tui::{Browser, Launcher};
use std::io::{self, IsTerminal};

pub(crate) struct Browse(pub(crate) BrowseArgs);

impl Browse {
    pub(crate) fn run(&self) -> Result<(), CliError> {
        if !io::stdout().is_terminal() {
            return Err(CliError::Usage {
                message: "browse requires standard output to be a terminal",
            });
        }
        let args = &self.0;
        let Some(source) = &args.source else {
            return Launcher::new(CliSources::new(args.limits.clone()))
                .run()
                .map_err(|source| CliError::Terminal { source });
        };
        let output = Source::open(source)?.load(&args.limits)?;
        let locator = Locator::new(&output.snapshot);
        let id = locator.resolve(args.path.as_deref().unwrap_or("/"))?;
        locator.directory(id)?;
        Browser::new(&output, id)
            .run()
            .map_err(|source| CliError::Terminal { source })
    }
}
