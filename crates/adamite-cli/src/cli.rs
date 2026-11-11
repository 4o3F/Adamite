use crate::{
    command::{info::Info, scan::Scan},
    error::CliError,
};
use clap::{Args, Parser, Subcommand};
use std::{io::Write, path::PathBuf};

#[derive(Debug, Parser)]
#[command(
    name = "adamite",
    version,
    about = "Scan NTFS volumes and inspect snapshots"
)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Scan an NTFS image or Windows drive.
    Scan(SourceArgs),
    /// Show volume, tree totals and scan diagnostics.
    Info(SourceArgs),
}

#[derive(Debug, Args)]
pub(crate) struct SourceArgs {
    /// Image path, snapshot path, or Windows drive such as C:.
    pub(crate) source: PathBuf,
    /// Print one JSON object with byte counts.
    #[arg(long)]
    pub(crate) json: bool,
    /// Show raw byte counts in text output.
    #[arg(long)]
    pub(crate) bytes: bool,
}

impl Command {
    pub(crate) fn run(self, stdout: &mut impl Write) -> Result<(), CliError> {
        match self {
            Self::Scan(args) => Scan(args).run(stdout),
            Self::Info(args) => Info(args).run(stdout),
        }
    }
}
