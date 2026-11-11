use super::{escape::Escaped, units::Size};
use crate::{
    error::CliError,
    report::{Envelope, ExtReport, InfoReport, LsReport, SavedReport, TopReport},
};
use std::io::{self, Write};

struct Header<'a>(&'a Envelope<'a>);

impl Header<'_> {
    fn write(&self, stdout: &mut impl Write) -> io::Result<()> {
        writeln!(
            stdout,
            "{} — scanned {}",
            Escaped(&self.0.snapshot.source),
            self.0.snapshot.scanned_at.as_deref().unwrap_or("unknown")
        )
    }
}

struct SizeColumn {
    bytes: bool,
}

impl SizeColumn {
    fn width(&self) -> usize {
        if self.bytes { 20 } else { 12 }
    }

    fn value(&self, value: u64) -> String {
        Size {
            value: u128::from(value),
            bytes: self.bytes,
        }
        .to_string()
    }
}

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
        Header(&report.envelope).write(stdout)?;
        writeln!(
            stdout,
            "  {:<24} {}",
            "Volume serial", report.envelope.snapshot.volume_serial
        )?;
        if let Some(label) = report.envelope.snapshot.label {
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
                value: u128::from(value),
                bytes: self.bytes
            }
        )
    }
}

pub(crate) struct TopText<'a> {
    pub(crate) report: &'a TopReport<'a>,
    pub(crate) bytes: bool,
}

impl TopText<'_> {
    pub(crate) fn write(&self, stdout: &mut impl Write) -> Result<(), CliError> {
        self.write_rows(stdout)
            .map_err(|source| CliError::Output { source })
    }

    fn write_rows(&self, stdout: &mut impl Write) -> io::Result<()> {
        Header(&self.report.envelope).write(stdout)?;
        let column = SizeColumn { bytes: self.bytes };
        let width = column.width();
        writeln!(stdout, "{:>width$} {:>width$} Path", "Size", "Alloc")?;
        for item in &self.report.items {
            writeln!(
                stdout,
                "{:>width$} {:>width$} {}",
                column.value(item.size),
                column.value(item.alloc),
                Escaped(&item.path)
            )?;
        }
        Ok(())
    }
}

pub(crate) struct ExtText<'a> {
    pub(crate) report: &'a ExtReport<'a>,
    pub(crate) bytes: bool,
}

impl ExtText<'_> {
    pub(crate) fn write(&self, stdout: &mut impl Write) -> Result<(), CliError> {
        self.write_rows(stdout)
            .map_err(|source| CliError::Output { source })
    }

    fn write_rows(&self, stdout: &mut impl Write) -> io::Result<()> {
        Header(&self.report.envelope).write(stdout)?;
        let column = SizeColumn { bytes: self.bytes };
        let width = column.width();
        writeln!(
            stdout,
            "{:>width$} {:>width$} {:>10} Extension",
            "Size", "Alloc", "Count"
        )?;
        for item in &self.report.items {
            let extension = if item.extension.is_empty() {
                "(none)"
            } else {
                item.extension
            };
            writeln!(
                stdout,
                "{:>width$} {:>width$} {:>10} {}",
                column.value(item.total_size),
                column.value(item.total_alloc),
                item.count,
                Escaped(extension)
            )?;
        }
        Ok(())
    }
}

pub(crate) struct LsText<'a> {
    pub(crate) report: &'a LsReport<'a>,
    pub(crate) bytes: bool,
}

impl LsText<'_> {
    pub(crate) fn write(&self, stdout: &mut impl Write) -> Result<(), CliError> {
        self.write_rows(stdout)
            .map_err(|source| CliError::Output { source })
    }

    fn write_rows(&self, stdout: &mut impl Write) -> io::Result<()> {
        Header(&self.report.envelope).write(stdout)?;
        let column = SizeColumn { bytes: self.bytes };
        let width = column.width();
        writeln!(
            stdout,
            "{:>width$} {:>width$} {:>10} Name",
            "Total size", "Total alloc", "Files"
        )?;
        for item in &self.report.items {
            let suffix = if item.kind == "dir" { "/" } else { "" };
            writeln!(
                stdout,
                "{:>width$} {:>width$} {:>10} {}{suffix}",
                column.value(item.total_size),
                column.value(item.total_alloc),
                item.file_count,
                Escaped(item.name)
            )?;
        }
        Ok(())
    }
}

pub(crate) struct SavedText<'a> {
    pub(crate) report: &'a SavedReport<'a>,
    pub(crate) bytes: bool,
}

impl SavedText<'_> {
    pub(crate) fn write(&self, stdout: &mut impl Write) -> Result<(), CliError> {
        writeln!(
            stdout,
            "saved {} ({})",
            Escaped(&self.report.saved),
            Size {
                value: self.report.bytes as u128,
                bytes: self.bytes
            },
        )
        .map_err(|source| CliError::Output { source })
    }
}
