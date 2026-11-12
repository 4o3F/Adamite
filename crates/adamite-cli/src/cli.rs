use crate::{
    command::{
        browse::Browse, ext::Ext, info::Info, ls::Ls, scan::Scan, search::Search, top::Top,
        tree::Tree,
    },
    error::CliError,
};
use adamite_access::{ReadRate, ScanOptions};
use adamite_core::snapshot::FileKind;
use adamite_io::limits::{ScanLimits, ScanProfile};
use clap::{
    Args, Parser, Subcommand, ValueEnum,
    builder::{PossibleValuesParser, TypedValueParser},
    error::{ContextKind, ContextValue},
};
use std::num::{NonZeroU64, NonZeroUsize};
use std::{ffi::OsStr, io::Write, path::PathBuf};

#[derive(Debug, Parser)]
#[command(
    name = "adamite",
    version,
    about = "Scan NTFS volumes and inspect snapshots"
)]
pub(crate) struct Cli {
    /// Write a Chrome trace with stage timings and requested heap usage.
    #[arg(long, global = true, value_name = "FILE")]
    pub(crate) trace_chrome: Option<PathBuf>,
    #[command(subcommand)]
    pub(crate) command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Scan an NTFS image or Windows drive.
    Scan(ScanArgs),
    /// Show volume, tree totals and scan diagnostics.
    Info(SourceArgs),
    /// Show the largest files, optionally filtered or under a directory.
    Top(TopArgs),
    /// Show file statistics grouped by extension or kind.
    Ext(ExtArgs),
    /// List a directory's children in size order.
    Ls(LsArgs),
    /// Show a budgeted tree of subtree allocation and shares.
    Tree(TreeArgs),
    /// Search node names with a case-insensitive glob or substring.
    Search(SearchArgs),
    /// Browse the directory tree interactively in a terminal.
    Browse(BrowseArgs),
}

#[derive(Debug, Args)]
pub(crate) struct SourceArgs {
    #[command(flatten)]
    pub(crate) limits: LimitArgs,
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
    /// Write scan progress and the saved outcome as JSON lines to a new file.
    #[arg(long, requires = "output", value_name = "FILE")]
    pub(crate) status_file: Option<PathBuf>,
    /// Replace an existing output file.
    #[arg(long, requires = "output")]
    pub(crate) force: bool,
}

#[derive(Debug, Args)]
pub(crate) struct TopArgs {
    #[command(flatten)]
    pub(crate) source: SourceArgs,
    /// Restrict files to this directory subtree; #id selects a direct child.
    #[arg(long, value_name = "PATH")]
    pub(crate) under: Option<String>,
    /// Match comma-separated extensions; may repeat; an empty value matches files without an extension.
    #[arg(
        long,
        value_delimiter = ',',
        conflicts_with = "kind",
        value_name = "EXT"
    )]
    pub(crate) ext: Vec<String>,
    /// Match a file category inferred from its extension.
    #[arg(long, value_parser = file_kind_parser(), value_name = "KIND")]
    pub(crate) kind: Option<FileKind>,
    /// Number of files to show (1..=100).
    #[arg(long, default_value_t = 20, allow_negative_numbers = true, value_parser = TopCountParser)]
    pub(crate) n: u32,
}

fn file_kind_parser() -> impl TypedValueParser<Value = FileKind> {
    PossibleValuesParser::new(FileKind::ALL.map(FileKind::name))
        .try_map(|name| name.parse::<FileKind>())
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
    /// Restrict statistics to this directory subtree; #id selects a direct child.
    #[arg(long, value_name = "PATH")]
    pub(crate) under: Option<String>,
    /// Group files by extension or category.
    #[arg(long, value_enum, default_value = "extension")]
    pub(crate) by: GroupBy,
    /// Number of groups to show (at least 1).
    #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u64).range(1..))]
    pub(crate) n: u64,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum GroupBy {
    Extension,
    Kind,
}

#[derive(Debug, Args)]
pub(crate) struct SearchArgs {
    #[command(flatten)]
    pub(crate) source: SourceArgs,
    /// Name glob, or a substring when it contains none of *?[{ (at most 256 bytes).
    pub(crate) pattern: String,
    /// Restrict the search to this directory subtree; #id selects a direct child.
    #[arg(long, value_name = "PATH")]
    pub(crate) under: Option<String>,
    /// Number of matches to skip.
    #[arg(long, default_value_t = 0)]
    pub(crate) offset: usize,
    /// Maximum number of matches to show (1..=500).
    #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u16).range(1..=500))]
    pub(crate) limit: u16,
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
pub(crate) struct TreeArgs {
    #[command(flatten)]
    pub(crate) source: SourceArgs,
    /// Directory path; #id selects a direct child by its decimal NodeId.
    #[arg(default_value = "/")]
    pub(crate) path: String,
    /// Maximum rows, including root and collapsed groups (1..=400).
    #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u16).range(1..=400))]
    pub(crate) budget: u16,
    /// Minimum allocation share of the requested root, as a percentage (0..=100).
    #[arg(long, default_value = "0.5", allow_negative_numbers = true, value_parser = ShareParser)]
    pub(crate) min_share: u32,
}

#[derive(Debug, Clone)]
struct ShareParser;

impl TypedValueParser for ShareParser {
    type Value = u32;

    fn parse_ref(
        &self,
        command: &clap::Command,
        _arg: Option<&clap::Arg>,
        value: &OsStr,
    ) -> Result<Self::Value, clap::Error> {
        let percent = value
            .to_str()
            .and_then(|value| value.parse::<f64>().ok())
            .ok_or_else(|| {
                clap::Error::raw(
                    clap::error::ErrorKind::ValueValidation,
                    "--min-share must be a finite percentage from 0 to 100",
                )
                .with_cmd(command)
            })?;
        if !percent.is_finite() || !(0.0..=100.0).contains(&percent) {
            return Err(clap::Error::raw(
                clap::error::ErrorKind::ValueValidation,
                "--min-share must be a finite percentage from 0 to 100",
            )
            .with_cmd(command));
        }
        Self::basis_points(percent).ok_or_else(|| {
            clap::Error::raw(
                clap::error::ErrorKind::ValueValidation,
                "--min-share cannot be represented in basis points",
            )
            .with_cmd(command)
        })
    }
}

impl ShareParser {
    fn basis_points(percent: f64) -> Option<u32> {
        if percent == 0.0 {
            return Some(0);
        }
        // Truncate decimal places so values such as 0.29 do not lose a basis point
        // through binary floating-point multiplication.
        let decimal = percent.to_string();
        let (whole, fraction) = decimal.split_once('.').unwrap_or((&decimal, ""));
        let mut basis_points = whole.parse::<u32>().ok()?;
        let mut digits = fraction.bytes();
        for _ in 0..2 {
            let digit = digits.next().unwrap_or(b'0').checked_sub(b'0')?;
            basis_points = basis_points
                .checked_mul(10)?
                .checked_add(u32::from(digit))?;
        }
        Some(basis_points)
    }
}

#[derive(Debug, Default, Args)]
pub(crate) struct BrowseArgs {
    #[command(flatten)]
    pub(crate) limits: LimitArgs,
    /// Image path, snapshot path, or Windows drive such as C:; omit to open Home.
    pub(crate) source: Option<PathBuf>,
    /// Directory path; #id selects a direct child by its decimal NodeId.
    #[arg(requires = "source")]
    pub(crate) path: Option<String>,
}

impl Command {
    pub(crate) fn run(self, stdout: &mut impl Write) -> Result<(), CliError> {
        match self {
            Self::Scan(args) => Scan(args).run(stdout),
            Self::Info(args) => Info(args).run(stdout),
            Self::Top(args) => Top(args).run(stdout),
            Self::Ext(args) => Ext(args).run(stdout),
            Self::Ls(args) => Ls(args).run(stdout),
            Self::Tree(args) => Tree(args).run(stdout),
            Self::Search(args) => Search(args).run(stdout),
            Self::Browse(args) => Browse(args).run(),
        }
    }
}

/// Resource ceilings used whenever a source needs scanning.
#[derive(Debug, Default, Clone, Args)]
pub(crate) struct LimitArgs {
    /// Resource profile: background, balanced or max. Ignored for snapshots.
    #[arg(long, value_name = "PROFILE")]
    profile: Option<ScanProfile>,
    /// Bytes per second; K/M/G use powers of 1024. Use --profile max to uncap.
    #[arg(long, value_name = "BYTES", value_parser = Self::parse_rate)]
    max_read_rate: Option<ReadRate>,
    /// Worker ceiling (at least 1); the current scanner uses one thread.
    #[arg(long, value_name = "N")]
    threads: Option<NonZeroUsize>,
    /// Scan drives without requesting administrator access.
    #[arg(long)]
    pub(crate) no_elevate: bool,
}

impl LimitArgs {
    pub(crate) fn limits(&self) -> ScanLimits {
        ScanOptions::from(self).limits()
    }

    fn parse_rate(value: &str) -> Result<ReadRate, String> {
        let bytes = parse_size::Config::new()
            .with_binary()
            .parse_size(value)
            .map_err(|error| error.to_string())?;
        let bytes = NonZeroU64::new(bytes).ok_or_else(|| {
            "rate must be positive; use --profile max for uncapped reads".to_owned()
        })?;
        Ok(ReadRate::new(value.to_owned(), bytes))
    }
}

impl From<&LimitArgs> for ScanOptions {
    fn from(args: &LimitArgs) -> Self {
        Self {
            profile: args.profile,
            max_read_rate: args.max_read_rate.clone(),
            threads: args.threads,
            no_elevate: args.no_elevate,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Cli, Command};
    use adamite_io::limits::ScanProfile;
    use clap::Parser;
    use std::error::Error;

    #[test]
    fn tree_percentages_round_down_to_basis_points() -> Result<(), Box<dyn Error>> {
        for (text, expected) in [
            ("0", 0),
            ("-0", 0),
            ("0.5", 50),
            ("0.509", 50),
            ("0.29", 29),
            ("99.999", 9999),
            ("100", 10_000),
            ("1e-2", 1),
            ("1e-300", 0),
        ] {
            let cli = Cli::try_parse_from(["adamite", "tree", "source.adm", "--min-share", text])?;
            let Some(Command::Tree(args)) = cli.command else {
                return Err("missing tree".into());
            };
            assert_eq!(args.min_share, expected, "{text}");
        }
        Ok(())
    }

    #[test]
    fn access_options_preserve_explicit_arguments_and_omit_parent_flags()
    -> Result<(), Box<dyn Error>> {
        let cli = Cli::try_parse_from([
            "adamite",
            "scan",
            "C:",
            "--profile",
            "balanced",
            "--max-read-rate",
            "128MiB",
            "--threads",
            "1",
            "--no-elevate",
            "--trace-chrome",
            "parent.json",
        ])?;
        let Some(Command::Scan(args)) = cli.command else {
            return Err("missing scan".into());
        };
        let options = adamite_access::ScanOptions::from(&args.source.limits);
        assert!(options.no_elevate);
        assert_eq!(
            options.child_arguments(),
            [
                "--profile",
                "balanced",
                "--max-read-rate",
                "128MiB",
                "--threads",
                "1",
            ]
            .map(std::ffi::OsString::from)
        );
        assert!(
            adamite_access::ScanOptions::from(&super::LimitArgs::default())
                .child_arguments()
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn every_source_command_shares_limits() -> Result<(), Box<dyn Error>> {
        for name in ["scan", "info", "top", "ext", "ls", "browse"] {
            let cli = Cli::try_parse_from([
                "adamite",
                name,
                "source.img",
                "--profile",
                "background",
                "--max-read-rate",
                "128MiB",
                "--threads",
                "7",
            ])?;
            let args = match cli.command.ok_or("missing command")? {
                Command::Scan(args) => args.source.limits,
                Command::Info(args) => args.limits,
                Command::Top(args) => args.source.limits,
                Command::Ext(args) => args.source.limits,
                Command::Ls(args) => args.source.limits,
                Command::Tree(args) => args.source.limits,
                Command::Browse(args) => args.limits,
                Command::Search(args) => args.source.limits,
            };
            let limits = args.limits();
            assert_eq!(
                limits.max_read_bytes_per_sec.map(|r| r.get()),
                Some(128 << 20)
            );
            assert_eq!(limits.threads.get(), 7);
            assert_eq!(limits.chunk_size, 1 << 20);
        }
        Ok(())
    }

    #[test]
    fn profile_defaults_names_and_binary_rate_suffixes() -> Result<(), Box<dyn Error>> {
        let cli = Cli::try_parse_from(["adamite", "info", "source.adm"])?;
        let Some(Command::Info(args)) = cli.command else {
            return Err("missing info".into());
        };
        assert_eq!(args.limits.limits(), ScanProfile::Balanced.limits());
        for (name, expected) in [
            ("background", ScanProfile::Background),
            ("balanced", ScanProfile::Balanced),
            ("max", ScanProfile::Max),
        ] {
            let cli = Cli::try_parse_from(["adamite", "scan", "source.img", "--profile", name])?;
            let Some(Command::Scan(args)) = cli.command else {
                return Err("missing scan".into());
            };
            assert_eq!(args.source.limits.limits(), expected.limits());
        }
        for (text, bytes) in [
            ("1", 1),
            ("2K", 2 << 10),
            ("3M", 3 << 20),
            ("4G", 4 << 30),
            ("2KiB", 2 << 10),
            ("3MiB", 3 << 20),
            ("4GiB", 4 << 30),
        ] {
            let cli =
                Cli::try_parse_from(["adamite", "scan", "source.img", "--max-read-rate", text])?;
            let Some(Command::Scan(args)) = cli.command else {
                return Err("missing scan".into());
            };
            assert_eq!(
                args.source
                    .limits
                    .limits()
                    .max_read_bytes_per_sec
                    .map(|r| r.get()),
                Some(bytes)
            );
        }
        Ok(())
    }

    #[test]
    fn invalid_limits_are_usage_errors() {
        for (flag, value) in [
            ("--profile", "fast"),
            ("--profile", "Max"),
            ("--threads", "0"),
            ("--threads", "-1"),
            ("--max-read-rate", "0"),
            ("--max-read-rate", "unlimited"),
            ("--max-read-rate", "-1M"),
            ("--max-read-rate", "999999999999999999999G"),
            ("--max-read-rate", "nonsense"),
        ] {
            let result = Cli::try_parse_from(["adamite", "scan", "source.img", flag, value]);
            let error = result
                .err()
                .unwrap_or_else(|| unreachable!("invalid limits accepted"));
            assert_eq!(error.exit_code(), 2);
        }
    }
}
