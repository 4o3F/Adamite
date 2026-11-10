//! Scans an NTFS image or Windows raw volume and prints its largest root children.

use adamite_core::{
    scan::{ScanConfig, ScanOutput},
    snapshot::NodeId,
};
use adamite_io::{driver::SyncDriver, error::VolumeError, reader::ImageReader};
use std::{
    env,
    error::Error,
    path::{Path, PathBuf},
    process::ExitCode,
};

enum Source {
    Image(PathBuf),
    Drive(char),
}

fn parse_source() -> Option<Source> {
    let mut args = env::args().skip(1);
    let first = args.next()?;
    match (first.as_str(), args.next(), args.next()) {
        ("--drive", Some(drive), None) => {
            let mut letters = drive.chars();
            let letter = letters.next()?;
            if letter.is_ascii_alphabetic() && letters.next().is_none() {
                Some(Source::Drive(letter))
            } else {
                None
            }
        }
        (_, None, None) if !first.is_empty() && !first.starts_with('-') => {
            Some(Source::Image(PathBuf::from(first)))
        }
        _ => None,
    }
}

fn scan_image(path: &Path) -> Result<ScanOutput, VolumeError> {
    let mut reader = ImageReader::open(path)?;
    SyncDriver::new(ScanConfig::default())?.run(&mut reader)
}

#[cfg(windows)]
fn scan_drive(letter: char) -> ExitCode {
    use adamite_io::reader::RawVolumeReader;

    let result = RawVolumeReader::open_drive(letter).and_then(|mut reader| {
        let config = ScanConfig {
            context: reader.context()?,
            ..ScanConfig::default()
        };
        SyncDriver::new(config)?.run(&mut reader)
    });
    report(result)
}

#[cfg(not(windows))]
fn scan_drive(_letter: char) -> ExitCode {
    eprintln!("raw volumes require Windows");
    ExitCode::from(2)
}

fn report(result: Result<ScanOutput, VolumeError>) -> ExitCode {
    match result {
        Ok(output) => {
            println!("ScanStats: {:?}", output.stats);
            let snapshot = output.snapshot;
            println!("VolumeInfo: {:?}", snapshot.volume());
            println!("Largest root children (bytes):");
            if let Some(children) = snapshot.children(NodeId::ROOT) {
                for &id in children.iter().take(10) {
                    if let (Some(name), Some(node)) = (snapshot.name(id), snapshot.node(id)) {
                        println!(
                            "  {name}: allocated={}, total_size={}",
                            node.total_alloc(),
                            node.total_size()
                        );
                    }
                }
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            let mut source = error.source();
            while let Some(cause) = source {
                eprintln!("  caused by: {cause}");
                source = cause.source();
            }
            ExitCode::FAILURE
        }
    }
}

fn main() -> ExitCode {
    match parse_source() {
        Some(Source::Image(path)) => report(scan_image(&path)),
        Some(Source::Drive(letter)) => scan_drive(letter),
        None => {
            eprintln!("usage: cargo run -p adamite-io --example scan -- <image-path> | --drive C");
            ExitCode::from(2)
        }
    }
}
