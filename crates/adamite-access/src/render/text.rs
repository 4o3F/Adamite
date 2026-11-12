use super::Report;
use crate::report::{
    Envelope, ExtReport, InfoReport, KindReport, LsItem, LsReport, SavedReport, SearchReport,
    TopReport, TreeReport,
};
use adamite_format::{ByteSize, Escaped, ExtensionLabel, InfoFields};
use std::io::{self, Write};

struct Header<'a>(&'a Envelope<'a>);

impl Header<'_> {
    fn write(&self, stdout: &mut impl Write) -> io::Result<()> {
        writeln!(
            stdout,
            "{} — scanned {}",
            Escaped(&self.0.snapshot.source),
            self.0.snapshot.scanned_at
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
}

struct SizeRow {
    size: u64,
    alloc: u64,
    count: Option<u64>,
    name: String,
}

impl SizeRow {
    fn node(item: &LsItem<'_>, name: &str) -> Self {
        Self {
            size: item.total_size,
            alloc: item.total_alloc,
            count: Some(u64::from(item.file_count)),
            name: format!(
                "{}{}",
                Escaped(name),
                if item.kind == "dir" { "/" } else { "" }
            ),
        }
    }
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
        for (label, value) in (InfoFields {
            output: self.output,
            raw_bytes,
        })
        .rows()
        {
            writeln!(stdout, "  {label:<24} {value}")?;
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
                name: ExtensionLabel(item.extension).to_string(),
            }),
        )
    }
}

impl Report for KindReport<'_> {
    fn write_text(&self, stdout: &mut impl Write, raw_bytes: bool) -> io::Result<()> {
        Header(&self.envelope).write(stdout)?;
        SizeTable {
            sizes: ["Size", "Alloc"],
            count: Some("Count"),
            name: "Kind",
            column: SizeColumn { raw_bytes },
        }
        .write(
            stdout,
            self.items.iter().map(|item| SizeRow {
                size: item.total_size,
                alloc: item.total_alloc,
                count: Some(item.count),
                name: item.kind.to_owned(),
            }),
        )
    }
}

impl Report for SearchReport<'_> {
    fn write_text(&self, stdout: &mut impl Write, raw_bytes: bool) -> io::Result<()> {
        Header(&self.envelope).write(stdout)?;
        SizeTable {
            sizes: ["Total size", "Total alloc"],
            count: Some("Files"),
            name: "Path",
            column: SizeColumn { raw_bytes },
        }
        .write(
            stdout,
            self.items
                .iter()
                .map(|item| SizeRow::node(&item.node, &item.path)),
        )?;
        if let Some((start, end)) = self.shown {
            write!(stdout, "showing {start}–{end} of {}", self.total_matches)?;
            if let Some(next) = self.next_offset {
                write!(stdout, " · next offset={next}")?;
            }
            writeln!(stdout)
        } else if self.total_matches == 0 {
            writeln!(stdout, "no matches")
        } else {
            writeln!(stdout, "showing 0 of {}", self.total_matches)
        }
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
            self.items.iter().map(|item| SizeRow::node(item, item.name)),
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

impl Report for TreeReport<'_> {
    fn write_text(&self, stdout: &mut impl Write, raw_bytes: bool) -> io::Result<()> {
        let snapshot = &self.envelope.snapshot;
        writeln!(
            stdout,
            "{} · serial {} · scanned {} ({:#}) · sizes: on-disk allocation",
            Escaped(&snapshot.source),
            snapshot.volume_serial,
            snapshot.scanned_at,
            snapshot.scanned_at.age(std::time::SystemTime::now())
        )?;
        let column = SizeColumn { raw_bytes };
        let width = column.width();
        writeln!(stdout, "{:>width$}  {:>6}  name", "alloc", "share")?;
        for row in &self.rows {
            write!(
                stdout,
                "{:>width$}  {:>5.1}%  ",
                column.value(row.alloc).to_string(),
                row.share
            )?;
            for _ in 0..row.depth {
                write!(stdout, "  ")?;
            }
            if let Some(count) = row.count {
                writeln!(stdout, "… {count} more ({})", column.value(row.alloc))?;
            } else {
                writeln!(stdout, "{}", Escaped(row.name))?;
            }
        }
        Ok(())
    }
}
