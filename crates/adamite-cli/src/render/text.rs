use super::{escape::Escaped, units::Size};
use crate::{error::CliError, report::InfoReport};
use std::io::{self, Write};

pub(crate) struct Text<'a> {
    pub(crate) report: &'a InfoReport<'a>,
    pub(crate) bytes: bool,
}

impl Text<'_> {
    pub(crate) fn write(&self, stdout: &mut impl Write) -> Result<(), CliError> {
        self.write_info(stdout)
            .map_err(|source| CliError::Output { source })
    }

    fn write_info(&self, stdout: &mut impl Write) -> io::Result<()> {
        let report = self.report;
        writeln!(
            stdout,
            "{} — scanned {}",
            Escaped(&report.snapshot.source),
            report.snapshot.scanned_at.as_deref().unwrap_or("unknown")
        )?;
        writeln!(
            stdout,
            "  {:<24} {}",
            "Volume serial", report.snapshot.volume_serial
        )?;
        if let Some(label) = report.snapshot.label {
            writeln!(stdout, "  {:<24} {}", "Label", Escaped(label))?;
        }
        self.size_line(
            stdout,
            "Bytes per sector",
            u64::from(report.volume.bytes_per_sector),
        )?;
        self.size_line(
            stdout,
            "Cluster size",
            u64::from(report.volume.cluster_size),
        )?;
        self.size_line(stdout, "Volume size", report.volume.total_bytes)?;
        if let Some(free) = report.volume.free_bytes {
            self.size_line(stdout, "Free space", free)?;
        }
        writeln!(stdout, "  {:<24} {}", "Nodes", report.tree.nodes)?;
        writeln!(stdout, "  {:<24} {}", "Files", report.tree.files)?;
        self.size_line(stdout, "Logical size", report.tree.total_size)?;
        self.size_line(stdout, "Allocated size", report.tree.total_alloc)?;
        for (key, value) in report.stats.entries() {
            if value != 0 {
                writeln!(stdout, "  {key:<24} {value}")?;
            }
        }
        Ok(())
    }

    fn size_line(&self, stdout: &mut impl Write, label: &str, value: u64) -> io::Result<()> {
        writeln!(
            stdout,
            "  {label:<24} {}",
            Size {
                value,
                bytes: self.bytes
            }
        )
    }
}
