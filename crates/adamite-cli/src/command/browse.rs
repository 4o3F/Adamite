use crate::{
    cli::BrowseArgs,
    error::{CliError, DirectoryLookup},
    launcher::CliSources,
    source::Source,
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
        let id = output
            .snapshot
            .resolve_directory(args.path.as_deref().unwrap_or("/"))?;
        Browser::new(&output, id)
            .run()
            .map_err(|source| CliError::Terminal { source })
    }
}
