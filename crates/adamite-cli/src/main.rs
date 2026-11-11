//! Scans NTFS images and Windows volumes, saves snapshots and reports their information.
#![forbid(unsafe_code)]
#![deny(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

mod cli;
mod command;
mod error;
mod locate;
mod render;
mod report;
mod save;
mod scan_progress_view;
mod source;

use adamite_profile::{ChromeProfile, CountingAllocator};
use clap::{CommandFactory, Parser};

use cli::Cli;
use std::{
    io::{self, Write},
    process::ExitCode,
};

#[global_allocator]
static ALLOC: CountingAllocator = CountingAllocator::new();

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            let code = if error.use_stderr() { 2 } else { 0 };
            let result = if error.use_stderr() {
                io::stderr().lock().write_all(error.to_string().as_bytes())
            } else {
                io::stdout().lock().write_all(error.to_string().as_bytes())
            };
            return match result {
                Err(error) if error.kind() != io::ErrorKind::BrokenPipe => ExitCode::FAILURE,
                _ => ExitCode::from(code),
            };
        }
    };
    let _profile = match cli
        .trace_chrome
        .as_ref()
        .map(|path| ChromeProfile::install(path, &ALLOC))
        .transpose()
    {
        Ok(profile) => profile,
        Err(source) => {
            let error = error::CliError::Profile { source };
            error.print();
            return ExitCode::from(error.exit_code());
        }
    };
    let mut stdout = io::stdout().lock();
    let result = match cli.command {
        Some(command) => command.run(&mut stdout),
        None => Cli::command()
            .write_long_help(&mut stdout)
            .and_then(|()| writeln!(stdout))
            .map_err(|source| error::CliError::Output { source }),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) if error.is_broken_pipe() => ExitCode::SUCCESS,
        Err(error) => {
            error.print();
            ExitCode::from(error.exit_code())
        }
    }
}
