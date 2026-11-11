use crate::{cli::BrowseArgs, error::CliError, locate::Locator, source::Source};
use adamite_tui::Browser;
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
        let output = Source::open(&args.source)?.load(args.limits.limits())?;
        let locator = Locator::new(&output.snapshot);
        let id = locator.resolve(&args.path)?;
        locator.directory(id)?;
        Browser::new(&output, id)
            .run()
            .map_err(|source| CliError::Terminal { source })
    }
}
