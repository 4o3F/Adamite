use super::Report;
use crate::report::{Envelope, ExtReport, InfoReport, LsReport, SavedReport, TopReport};
use adamite_format::{ByteSize, Escaped};
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
    raw_bytes: bool,
}

impl SizeColumn {
    fn width(&self) -> usize {
        if self.raw_bytes { 20 } else { 12 }
    }

    fn value(&self, value: u64) -> ByteSize {
        ByteSize {
            value: u128::from(value),
            raw_bytes: self.raw_bytes,
        }
    }

    fn write_line(&self, stdout: &mut impl Write, label: &str, value: u64) -> io::Result<()> {
        writeln!(stdout, "  {label:<24} {}", self.value(value))
    }
}

struct SizeRow {
    size: u64,
    alloc: u64,
    count: Option<u64>,
    name: String,
}

struct SizeTable<'a> {
    sizes: [&'a str; 2],
    count: Option<&'a str>,
    name: &'a str,
    column: SizeColumn,
}

impl SizeTable<'_> {
    fn write(
        &self,
        stdout: &mut impl Write,
        rows: impl IntoIterator<Item = SizeRow>,
    ) -> io::Result<()> {
        let [size, alloc] = self.sizes;
        let width = self.column.width();
        write!(stdout, "{size:>width$} {alloc:>width$}")?;
        if let Some(count) = self.count {
            write!(stdout, " {count:>10}")?;
        }
        writeln!(stdout, " {}", self.name)?;
        for row in rows {
            // Materialize values so padding applies to the complete formatted unit.
            write!(
                stdout,
                "{:>width$} {:>width$}",
                self.column.value(row.size).to_string(),
                self.column.value(row.alloc).to_string()
            )?;
            if let Some(count) = row.count {
                write!(stdout, " {count:>10}")?;
            }
            writeln!(stdout, " {}", row.name)?;
        }
        Ok(())
    }
}

impl Report for InfoReport<'_> {
    fn write_text(&self, stdout: &mut impl Write, raw_bytes: bool) -> io::Result<()> {
        Header(&self.envelope).write(stdout)?;
        writeln!(
            stdout,
            "  {:<24} {}",
            "Volume serial", self.envelope.snapshot.volume_serial
        )?;
        if let Some(label) = self.envelope.snapshot.label {
            writeln!(stdout, "  {:<24} {}", "Label", Escaped(label))?;
        }
        let column = SizeColumn { raw_bytes };
        for (label, value) in [
            ("Bytes per sector", u64::from(self.volume.bytes_per_sector)),
            ("Cluster size", u64::from(self.volume.cluster_size)),
            ("Volume size", self.volume.total_bytes),
        ] {
            column.write_line(stdout, label, value)?;
        }
        if let Some(free) = self.volume.free_bytes {
            column.write_line(stdout, "Free space", free)?;
        }
        writeln!(stdout, "  {:<24} {}", "Nodes", self.tree.nodes)?;
        writeln!(stdout, "  {:<24} {}", "Files", self.tree.files)?;
        column.write_line(stdout, "Logical size", self.tree.total_size)?;
        column.write_line(stdout, "Allocated size", self.tree.total_alloc)?;
        for (key, value) in self.stats.entries() {
            if value != 0 {
                writeln!(stdout, "  {key:<24} {value}")?;
            }
        }
        Ok(())
    }
}

impl Report for TopReport<'_> {
    fn write_text(&self, stdout: &mut impl Write, raw_bytes: bool) -> io::Result<()> {
        Header(&self.envelope).write(stdout)?;
        SizeTable {
            sizes: ["Size", "Alloc"],
            count: None,
            name: "Path",
            column: SizeColumn { raw_bytes },
        }
        .write(
            stdout,
            self.items.iter().map(|item| SizeRow {
                size: item.size,
                alloc: item.alloc,
                count: None,
                name: Escaped(&item.path).to_string(),
            }),
        )
    }
}

impl Report for ExtReport<'_> {
    fn write_text(&self, stdout: &mut impl Write, raw_bytes: bool) -> io::Result<()> {
        Header(&self.envelope).write(stdout)?;
        SizeTable {
            sizes: ["Size", "Alloc"],
            count: Some("Count"),
            name: "Extension",
            column: SizeColumn { raw_bytes },
        }
        .write(
            stdout,
            self.items.iter().map(|item| SizeRow {
                size: item.total_size,
                alloc: item.total_alloc,
                count: Some(item.count),
                name: Escaped(if item.extension.is_empty() {
                    "(none)"
                } else {
                    item.extension
                })
                .to_string(),
            }),
        )
    }
}

impl Report for LsReport<'_> {
    fn write_text(&self, stdout: &mut impl Write, raw_bytes: bool) -> io::Result<()> {
        Header(&self.envelope).write(stdout)?;
        SizeTable {
            sizes: ["Total size", "Total alloc"],
            count: Some("Files"),
            name: "Name",
            column: SizeColumn { raw_bytes },
        }
        .write(
            stdout,
            self.items.iter().map(|item| SizeRow {
                size: item.total_size,
                alloc: item.total_alloc,
                count: Some(u64::from(item.file_count)),
                name: format!(
                    "{}{}",
                    Escaped(item.name),
                    if item.kind == "dir" { "/" } else { "" }
                ),
            }),
        )
    }
}

impl Report for SavedReport<'_> {
    fn write_text(&self, stdout: &mut impl Write, raw_bytes: bool) -> io::Result<()> {
        writeln!(
            stdout,
            "saved {} ({})",
            Escaped(&self.saved),
            ByteSize {
                value: self.bytes as u128,
                raw_bytes
            }
        )
    }
}
