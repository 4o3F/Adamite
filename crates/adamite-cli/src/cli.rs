use crate::{
    command::{browse::Browse, ext::Ext, info::Info, ls::Ls, scan::Scan, top::Top},
    error::CliError,
};
use clap::{
    Args, Parser, Subcommand,
    builder::TypedValueParser,
    error::{ContextKind, ContextValue},
};
use std::{ffi::OsStr, io::Write, path::PathBuf};

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
    Scan(ScanArgs),
    /// Show volume, tree totals and scan diagnostics.
    Info(SourceArgs),
    /// Show files from the precomputed 100-entry index.
    Top(TopArgs),
    /// Show extension statistics in stored order.
    Ext(ExtArgs),
    /// List a directory's children in size order.
    Ls(LsArgs),
    /// Browse the directory tree interactively in a terminal.
    Browse(BrowseArgs),
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

#[derive(Debug, Args)]
pub(crate) struct ScanArgs {
    #[command(flatten)]
    pub(crate) source: SourceArgs,
    /// Save the scan as a snapshot file.
    #[arg(short, long, value_name = "FILE")]
    pub(crate) output: Option<PathBuf>,
    /// Replace an existing output file.
    #[arg(long, requires = "output")]
    pub(crate) force: bool,
}

#[derive(Debug, Args)]
pub(crate) struct TopArgs {
    #[command(flatten)]
    pub(crate) source: SourceArgs,
    /// Number of files from the 100-entry index (1..=100).
    #[arg(long, default_value_t = 20, allow_negative_numbers = true, value_parser = TopCountParser)]
    pub(crate) n: u32,
}

#[derive(Debug, Clone)]
struct TopCountParser;

impl TypedValueParser for TopCountParser {
    type Value = u32;

    fn parse_ref(
        &self,
        command: &clap::Command,
        arg: Option<&clap::Arg>,
        value: &OsStr,
    ) -> Result<Self::Value, clap::Error> {
        clap::value_parser!(u32)
            .range(1..=100)
            .parse_ref(command, arg, value)
            .map_err(|mut error| {
                error.insert(
                    ContextKind::Suggested,
                    ContextValue::StyledStrs(vec!["top uses a precomputed 100-entry index".into()]),
                );
                error
            })
    }
}

#[derive(Debug, Args)]
pub(crate) struct ExtArgs {
    #[command(flatten)]
    pub(crate) source: SourceArgs,
    /// Number of extension groups to show (at least 1).
    #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u64).range(1..))]
    pub(crate) n: u64,
}

#[derive(Debug, Args)]
pub(crate) struct LsArgs {
    #[command(flatten)]
    pub(crate) source: SourceArgs,
    /// Directory path; #id selects a direct child by its decimal NodeId.
    #[arg(default_value = "/")]
    pub(crate) path: String,
    /// Number of children to skip.
    #[arg(long, default_value_t = 0)]
    pub(crate) offset: usize,
    /// Maximum number of children to show.
    #[arg(long, default_value_t = 50)]
    pub(crate) limit: usize,
}

#[derive(Debug, Args)]
pub(crate) struct BrowseArgs {
    /// Image path, snapshot path, or Windows drive such as C:.
    pub(crate) source: PathBuf,
    /// Directory path; #id selects a direct child by its decimal NodeId.
    #[arg(default_value = "/")]
    pub(crate) path: String,
}

impl Command {
    pub(crate) fn run(self, stdout: &mut impl Write) -> Result<(), CliError> {
        match self {
            Self::Scan(args) => Scan(args).run(stdout),
            Self::Info(args) => Info(args).run(stdout),
            Self::Top(args) => Top(args).run(stdout),
            Self::Ext(args) => Ext(args).run(stdout),
            Self::Ls(args) => Ls(args).run(stdout),
            Self::Browse(args) => Browse(args).run(),
        }
    }
}
