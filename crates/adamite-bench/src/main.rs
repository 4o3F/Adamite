//! Synthetic production-path scan and persistence resource measurements.

use adamite_core::scan::{ScanConfig, ScanOutput};
use adamite_fixtures::{SyntheticConfig, SyntheticVolume};
use adamite_io::{
    driver::SyncDriver,
    error::VolumeError,
    reader::{ImageReader, VolumeReader},
};
use adamite_profile::{ChromeProfile, CountingAllocator, SpanRecorder, SpanSummary};
use clap::Parser;
use std::{
    error::Error,
    fs,
    io::{self, BufWriter, Write},
    path::{Path, PathBuf},
    process::{Command, ExitCode},
    time::Instant,
};
use tempfile::NamedTempFile;
use tracing_subscriber::{Registry, layer::SubscriberExt};

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator::new();

type Result<T> = std::result::Result<T, Box<dyn Error>>;

/// Each stage is already instrumented by the production scanner or persistence codec.
const STAGES: [&str; 9] = [
    "bootstrap",
    "ingest",
    "merge",
    "link",
    "build",
    "aggregate",
    "finalize",
    "persist_encode",
    "persist_decode",
];

#[derive(Debug, Parser)]
#[command(about = "Measure scan and persistence on sparse synthetic NTFS images")]
struct Bench {
    /// In-use record counts; repeat the option or separate counts with commas.
    #[arg(long, value_delimiter = ',', default_values_t = [300_000u32, 1_000_000])]
    in_use: Vec<u32>,
    /// Directory for reusable images, keyed by the complete versioned configuration.
    #[arg(long, default_value = "target/bench")]
    output_dir: PathBuf,
    /// Chrome trace destination; multiple cases append the in-use count to its filename.
    #[arg(long)]
    trace_chrome: Option<PathBuf>,
    #[arg(long, hide = true)]
    case: Option<u32>,
}

#[derive(Debug)]
struct CountingReader {
    inner: ImageReader,
    bytes: u64,
}

impl VolumeReader for CountingReader {
    fn len_bytes(&self) -> u64 {
        self.inner.len_bytes()
    }
    fn read_exact_at(
        &mut self,
        offset: u64,
        buffer: &mut [u8],
    ) -> std::result::Result<(), VolumeError> {
        self.inner.read_exact_at(offset, buffer)?;
        self.bytes += buffer.len() as u64;
        Ok(())
    }
}

impl Bench {
    fn image(&self, used: u32) -> Result<PathBuf> {
        let config = SyntheticConfig::reference(used)?;
        let path = self.output_dir.join(config.image_name());
        let volume = SyntheticVolume::new(config)?;
        if fs::metadata(&path).is_ok_and(|metadata| metadata.len() == volume.len_bytes()) {
            return Ok(path);
        }
        fs::create_dir_all(&self.output_dir)?;
        let mut temporary = NamedTempFile::new_in(&self.output_dir)?;
        {
            let mut writer = BufWriter::with_capacity(1 << 20, temporary.as_file_mut());
            volume.write(&mut writer)?;
            writer.flush()?;
        }
        temporary.as_file().sync_all()?;
        temporary.persist(&path)?;
        Ok(path)
    }

    fn run(&self) -> Result<()> {
        if let Some(used) = self.case {
            return self.measure(used);
        }
        for &used in &self.in_use {
            self.image(used)?;
            let mut child = Command::new(std::env::current_exe()?);
            child
                .arg("--case")
                .arg(used.to_string())
                .arg("--output-dir")
                .arg(&self.output_dir);
            if let Some(path) = &self.trace_chrome {
                let path = self.trace_path(path, used);
                child.arg("--trace-chrome").arg(path);
            }
            let status = child.status()?;
            if !status.success() {
                return Err(io::Error::other(format!("case {used} failed: {status}")).into());
            }
        }
        Ok(())
    }

    fn trace_path(&self, path: &Path, used: u32) -> PathBuf {
        if self.in_use.len() == 1 {
            return path.to_owned();
        }
        let mut name = path.file_stem().unwrap_or_default().to_os_string();
        name.push(format!("-{used}."));
        name.push(
            path.extension()
                .unwrap_or_else(|| std::ffi::OsStr::new("json")),
        );
        path.with_file_name(name)
    }

    fn measure(&self, used: u32) -> Result<()> {
        let config = SyntheticConfig::reference(used)?;
        let path = self.output_dir.join(config.image_name());
        let mut reader = CountingReader {
            inner: ImageReader::open(&path)?,
            bytes: 0,
        };
        let recorder = SpanRecorder::new(&[STAGES.as_slice(), &["bench_case"]].concat());
        let profile = if let Some(path) = &self.trace_chrome {
            Some(ChromeProfile::install_with_recorder(
                path, &ALLOCATOR, &recorder,
            )?)
        } else {
            tracing::subscriber::set_global_default(
                Registry::default().with(recorder.layer(&ALLOCATOR)),
            )?;
            None
        };
        let case = tracing::info_span!("bench_case");
        let started = Instant::now();
        let output = SyncDriver::new(ScanConfig::default())?.run(&mut reader)?;
        let snapshot_heap = ALLOCATOR.current();
        let bytes = output.to_bytes()?;
        // Match save followed by load: the original scan output is released before decoding.
        drop(output);
        let decoded = ScanOutput::from_bytes(&bytes)?;
        std::hint::black_box(&decoded);
        let elapsed = started.elapsed();
        drop(case);
        let summaries = recorder.take();
        let overall = summaries
            .iter()
            .find(|summary| summary.name == "bench_case")
            .ok_or("missing overall span")?;
        println!(
            "\n| slots | in-use | MFT bytes | bytes read | elapsed ms | overall peak heap bytes | final snapshot heap bytes |"
        );
        println!("|---:|---:|---:|---:|---:|---:|---:|");
        println!(
            "| {} | {} | {} | {} | {:.3} | {} | {} |",
            config.slot_count,
            used,
            u64::from(config.slot_count) * 1024,
            reader.bytes,
            elapsed.as_secs_f64() * 1000.0,
            overall.heap_peak,
            snapshot_heap
        );
        println!("\n| stage | duration ms | own peak heap bytes | heap delta bytes |");
        println!("|---|---:|---:|---:|");
        for name in STAGES {
            let stage = Self::stage(&summaries, name)?;
            println!(
                "| {} | {:.3} | {} | {} |",
                stage.name,
                stage.duration.as_secs_f64() * 1000.0,
                stage.heap_peak,
                stage.heap_delta
            );
        }
        drop(profile);
        Ok(())
    }

    fn stage<'a>(summaries: &'a [SpanSummary], name: &str) -> Result<&'a SpanSummary> {
        let mut matching = summaries.iter().filter(|summary| summary.name == name);
        let result = matching
            .next()
            .ok_or_else(|| format!("missing stage {name}"))?;
        if matching.next().is_some() {
            return Err(format!("duplicate stage {name}").into());
        }
        Ok(result)
    }
}

fn main() -> ExitCode {
    match Bench::parse().run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Bench, CountingReader, Result};
    use adamite_core::scan::{ScanConfig, ScanOutput};
    use adamite_fixtures::SyntheticConfig;
    use adamite_io::{driver::SyncDriver, reader::ImageReader};
    use clap::Parser;
    use std::fs;

    #[test]
    fn tiny_cached_image_scans_and_round_trips_without_reading_payloads() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let bench = Bench {
            in_use: vec![202],
            output_dir: directory.path().to_owned(),
            trace_chrome: None,
            case: None,
        };
        let path = bench.image(202)?;
        let modified = fs::metadata(&path)?.modified()?;
        assert_eq!(bench.image(202)?, path);
        assert_eq!(
            fs::metadata(&path)?.modified()?,
            modified,
            "matching cache must be reused"
        );
        let mut reader = CountingReader {
            inner: ImageReader::open(&path)?,
            bytes: 0,
        };
        let output = SyncDriver::new(ScanConfig::default())?.run(&mut reader)?;
        let config = SyntheticConfig::reference(202)?;
        assert_eq!(output.stats.records_total, u64::from(config.slot_count));
        assert_eq!(output.stats.records_in_use, 202);
        assert_eq!(output.stats.corrupt_records, 0);
        assert_eq!(output.stats.orphans, 0);
        assert_eq!(
            reader.bytes,
            u64::from(config.slot_count) * 1024 + 512 + 1024
        );
        let bytes = output.to_bytes()?;
        assert_eq!(ScanOutput::from_bytes(&bytes)?, output);
        // A truncated cache must be rebuilt rather than reused.
        fs::OpenOptions::new()
            .write(true)
            .open(&path)?
            .set_len(512)?;
        bench.image(202)?;
        assert!(fs::metadata(&path)?.len() > 512);
        Ok(())
    }

    #[test]
    fn concurrent_generators_publish_complete_images() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let bench = Bench {
            in_use: vec![202],
            output_dir: directory.path().to_owned(),
            trace_chrome: None,
            case: None,
        };
        let barrier = std::sync::Barrier::new(2);
        let paths = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..2)
                .map(|_| {
                    scope.spawn(|| {
                        barrier.wait();
                        bench.image(202).map_err(|error| error.to_string())
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().expect("generator thread panicked"))
                .collect::<Vec<_>>()
        });
        assert_eq!(paths[0], paths[1]);
        let path = paths[0].as_ref().map_err(|error| error.to_owned())?;
        let mut reader = ImageReader::open(path)?;
        let output = SyncDriver::new(ScanConfig::default())?.run(&mut reader)?;
        assert_eq!(output.stats.records_in_use, 202);
        assert_eq!(
            fs::read_dir(directory.path())?.count(),
            1,
            "temporary files must be removed"
        );
        Ok(())
    }

    #[test]
    fn parses_multiple_counts_and_trace_destinations() -> Result<()> {
        let defaults = Bench::try_parse_from(["bench"])?;
        assert_eq!(defaults.in_use, vec![300_000, 1_000_000]);
        let one =
            Bench::try_parse_from(["bench", "--in-use", "202", "--trace-chrome", "trace.json"])?;
        assert_eq!(
            one.trace_path(std::path::Path::new("trace.json"), 202),
            std::path::Path::new("trace.json")
        );
        let many = Bench::try_parse_from(["bench", "--in-use", "202,402", "--in-use", "602"])?;
        assert_eq!(many.in_use, vec![202, 402, 602]);
        assert_eq!(
            many.trace_path(std::path::Path::new("trace.json"), 202),
            std::path::Path::new("trace-202.json")
        );
        Ok(())
    }
}
