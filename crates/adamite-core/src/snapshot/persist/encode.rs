//! Bounded canonical encoding with a checksum patched after the last chunk.

use super::{
    PersistError,
    format::{self, Crc32, SectionKind, Writer},
};
use crate::{
    scan::ScanStats,
    snapshot::{Node, Snapshot, VolumeInfo, storage::SnapshotStorage},
};

#[derive(Debug, Clone, Copy)]
struct Section {
    kind: SectionKind,
    offset: usize,
    len: usize,
    count: usize,
}

impl Section {
    fn array(kind: SectionKind, count: usize, width: usize) -> Result<Self, PersistError> {
        let len = count.checked_mul(width).ok_or(PersistError::Overflow {
            operation: "section length",
        })?;
        Ok(Self {
            kind,
            offset: 0,
            len,
            count,
        })
    }

    fn optional(kind: SectionKind, count: usize, width: usize) -> Result<Self, PersistError> {
        let bitmap_len = count.checked_add(7).ok_or(PersistError::Overflow {
            operation: "bitmap length",
        })? / 8;
        let mut section = Self::array(kind, count, width)?;
        section.len = section
            .len
            .checked_add(Writer::align_up(bitmap_len, 8)?)
            .ok_or(PersistError::Overflow {
                operation: "optional section length",
            })?;
        Ok(section)
    }

    fn end(self) -> Result<usize, PersistError> {
        self.offset
            .checked_add(self.len)
            .ok_or(PersistError::Overflow {
                operation: "section end",
            })
    }
}

#[derive(Debug)]
struct Layout {
    sections: [Section; format::SECTION_COUNT],
    file_len: usize,
}

impl Layout {
    fn new(data: &SnapshotStorage) -> Result<Self, PersistError> {
        Self::u32(data.nodes.len(), "node count")?;
        Self::u32(data.names.len(), "name pool length")?;
        let label_len = data.volume.context.label.as_ref().map_or(0, String::len);
        Self::u32(label_len, "label length")?;
        let volume_len =
            format::VOLUME_FIXED_LEN
                .checked_add(label_len)
                .ok_or(PersistError::Overflow {
                    operation: "volume length",
                })?;
        let ext_names_len = data.ext_stats.iter().try_fold(0usize, |len, group| {
            Self::u32(group.extension.len(), "extension length")?;
            len.checked_add(group.extension.len())
                .ok_or(PersistError::Overflow {
                    operation: "extension pool length",
                })
        })?;
        Self::u32(ext_names_len, "extension pool length")?;
        let mut sections = [
            Section {
                kind: SectionKind::Volume,
                offset: 0,
                len: volume_len,
                count: 1,
            },
            Section::array(SectionKind::Stats, format::STATS_COUNT, 8)?,
            Section::array(SectionKind::Nodes, data.nodes.len(), 64)?,
            Section::array(SectionKind::Names, data.names.len(), 1)?,
            Section::array(SectionKind::Children, data.children.len(), 4)?,
            Section::array(SectionKind::AdsSize, data.ads_size.len(), 8)?,
            Section::optional(SectionKind::FileRefs, data.file_refs.len(), 8)?,
            Section::array(SectionKind::Attrs, data.attrs.len(), 4)?,
            Section::array(SectionKind::Nlink, data.nlink.len(), 2)?,
            Section::optional(SectionKind::ReparseTags, data.reparse_tags.len(), 4)?,
            Section::array(SectionKind::TopFiles, data.top_files.len(), 4)?,
            Section::array(SectionKind::ExtStats, data.ext_stats.len(), 32)?,
            Section::array(SectionKind::ExtNames, ext_names_len, 1)?,
        ];
        let mut end = format::TABLE_END;
        for section in &mut sections {
            section.offset = Writer::align_up(end, 8)?;
            end = section.end()?;
            Self::u64(section.offset)?;
            Self::u64(section.len)?;
            Self::u64(section.count)?;
        }
        Self::u64(end)?;
        Ok(Self {
            sections,
            file_len: end,
        })
    }

    fn u32(value: usize, operation: &'static str) -> Result<u32, PersistError> {
        u32::try_from(value).map_err(|_| PersistError::TooLarge { operation })
    }

    fn u64(value: usize) -> Result<u64, PersistError> {
        u64::try_from(value).map_err(|_| PersistError::TooLarge {
            operation: "section size or offset",
        })
    }

    fn write_header(&self, writer: &mut Writer<'_>) -> Result<(), PersistError> {
        writer.bytes(format::MAGIC)?;
        writer.u32(format::VERSION)?;
        writer.u32(Self::u32(format::HEADER_LEN, "header length")?)?;
        writer.u64(Self::u64(self.file_len)?)?;
        writer.u32(Self::u32(format::SECTION_COUNT, "section count")?)?;
        writer.u32(0)?;
        writer.u32(0)?;
        writer.pad_to(format::HEADER_LEN)?;
        for section in self.sections {
            writer.u32(section.kind as u32)?;
            writer.u32(0)?;
            writer.u64(Self::u64(section.offset)?)?;
            writer.u64(Self::u64(section.len)?)?;
            writer.u64(Self::u64(section.count)?)?;
        }
        Ok(())
    }
}

/// Produces canonical file chunks using at most 1 MiB of reusable scratch storage.
///
/// The checksum slot is zero in the chunks. Patch it with [`Self::checksum_patch`]
/// after consuming the stream. Name pools and labels are borrowed directly.
#[derive(Debug)]
pub struct SnapshotEncoder<'a> {
    snapshot: &'a Snapshot,
    stats: &'a ScanStats,
    layout: Layout,
    scratch: Vec<u8>,
    section: usize,
    position: usize,
    file_position: usize,
    extension: usize,
    extension_offset: usize,
    crc: Crc32,
}

impl<'a> SnapshotEncoder<'a> {
    pub(crate) fn new(snapshot: &'a Snapshot, stats: &'a ScanStats) -> Result<Self, PersistError> {
        let layout = Layout::new(snapshot.data())?;
        let mut scratch = Vec::new();
        scratch.try_reserve_exact(Self::CHUNK_SIZE)?;
        scratch.resize(Self::CHUNK_SIZE, 0);
        Ok(Self {
            snapshot,
            stats,
            layout,
            scratch,
            section: 0,
            position: 0,
            file_position: 0,
            extension: 0,
            extension_offset: 0,
            crc: Crc32::new(),
        })
    }

    /// Maximum size of an emitted chunk and of the reusable scratch buffer.
    pub const CHUNK_SIZE: usize = 1 << 20;

    /// Complete encoded file length, including header and section table.
    pub fn file_len(&self) -> usize {
        self.layout.file_len
    }

    /// Returns the checksum slot and final little-endian checksum after the stream ends.
    pub fn checksum_patch(&self) -> Option<(usize, [u8; 4])> {
        (self.file_position == self.layout.file_len)
            .then(|| (32, self.crc.clone().finish().to_le_bytes()))
    }

    /// Produces the next file chunk, valid until the next mutable borrow.
    ///
    /// # Errors
    /// Returns a persistence error on checked arithmetic or format-width failure.
    pub fn next_chunk(&mut self) -> Result<Option<&[u8]>, PersistError> {
        if self.file_position == self.layout.file_len {
            return Ok(None);
        }
        let data = self.snapshot.data();
        let bytes = if self.file_position == 0 {
            let bytes =
                self.scratch
                    .get_mut(..format::TABLE_END)
                    .ok_or(PersistError::Overflow {
                        operation: "header buffer",
                    })?;
            self.layout.write_header(&mut Writer::new(bytes))?;
            bytes
        } else {
            let mut section =
                *self
                    .layout
                    .sections
                    .get(self.section)
                    .ok_or(PersistError::Overflow {
                        operation: "encoder section",
                    })?;
            while self.position == section.len && self.file_position >= section.offset {
                self.section = self.section.checked_add(1).ok_or(PersistError::Overflow {
                    operation: "encoder section",
                })?;
                self.position = 0;
                self.extension = 0;
                self.extension_offset = 0;
                section =
                    *self
                        .layout
                        .sections
                        .get(self.section)
                        .ok_or(PersistError::Overflow {
                            operation: "encoder section",
                        })?;
            }
            if self.file_position < section.offset {
                let len = section.offset.checked_sub(self.file_position).ok_or(
                    PersistError::Overflow {
                        operation: "section padding",
                    },
                )?;
                let bytes = self.scratch.get_mut(..len).ok_or(PersistError::Overflow {
                    operation: "padding buffer",
                })?;
                bytes.fill(0);
                bytes
            } else {
                let remaining =
                    section
                        .len
                        .checked_sub(self.position)
                        .ok_or(PersistError::Overflow {
                            operation: "section remaining",
                        })?;
                let len = remaining.min(Self::CHUNK_SIZE);
                let borrowed = match section.kind {
                    SectionKind::Names => Some(data.names.as_slice()),
                    SectionKind::Volume if self.position >= format::VOLUME_FIXED_LEN => Some(
                        data.volume
                            .context
                            .label
                            .as_deref()
                            .unwrap_or("")
                            .as_bytes(),
                    ),
                    SectionKind::ExtNames => {
                        while data
                            .ext_stats
                            .get(self.extension)
                            .is_some_and(|group| self.extension_offset == group.extension.len())
                        {
                            self.extension =
                                self.extension
                                    .checked_add(1)
                                    .ok_or(PersistError::Overflow {
                                        operation: "extension index",
                                    })?;
                            self.extension_offset = 0;
                        }
                        Some(
                            data.ext_stats
                                .get(self.extension)
                                .ok_or(PersistError::Overflow {
                                    operation: "extension name",
                                })?
                                .extension
                                .as_bytes(),
                        )
                    }
                    _ => None,
                };
                let bytes = if let Some(pool) = borrowed {
                    let start = match section.kind {
                        SectionKind::Volume => self.position.checked_sub(format::VOLUME_FIXED_LEN),
                        SectionKind::ExtNames => Some(self.extension_offset),
                        _ => Some(self.position),
                    }
                    .ok_or(PersistError::Overflow {
                        operation: "pool offset",
                    })?;
                    let bytes = pool.get(start..).ok_or(PersistError::Overflow {
                        operation: "pool range",
                    })?;
                    let bytes =
                        bytes
                            .get(..bytes.len().min(len))
                            .ok_or(PersistError::Overflow {
                                operation: "pool chunk",
                            })?;
                    if matches!(section.kind, SectionKind::ExtNames) {
                        self.extension_offset = self
                            .extension_offset
                            .checked_add(bytes.len())
                            .ok_or(PersistError::Overflow {
                                operation: "extension offset",
                            })?;
                    }
                    bytes
                } else {
                    let len = match section.kind {
                        SectionKind::Volume => format::VOLUME_FIXED_LEN,
                        SectionKind::Stats => section.len,
                        SectionKind::FileRefs | SectionKind::ReparseTags => {
                            let bitmap = section.count.div_ceil(8);
                            let values = Writer::align_up(bitmap, 8)?;
                            if self.position < values {
                                len.min(values.checked_sub(self.position).ok_or(
                                    PersistError::Overflow {
                                        operation: "bitmap remaining",
                                    },
                                )?)
                            } else {
                                len
                            }
                        }
                        _ => len,
                    };
                    let bytes = self.scratch.get_mut(..len).ok_or(PersistError::Overflow {
                        operation: "encode buffer",
                    })?;
                    Writer::new(bytes).section(
                        data,
                        self.stats,
                        section.kind,
                        self.position,
                        len,
                        &mut self.extension_offset,
                    )?;
                    bytes
                };
                self.position =
                    self.position
                        .checked_add(bytes.len())
                        .ok_or(PersistError::Overflow {
                            operation: "encoder position",
                        })?;
                bytes
            }
        };
        self.crc.update(bytes)?;
        self.file_position =
            self.file_position
                .checked_add(bytes.len())
                .ok_or(PersistError::Overflow {
                    operation: "file position",
                })?;
        Ok(Some(bytes))
    }
}

impl Writer<'_> {
    fn volume_header(&mut self, volume: &VolumeInfo) -> Result<(), PersistError> {
        let fields = volume.geometry.fields();
        self.u16(fields.bytes_per_sector)?;
        self.u32(fields.sectors_per_cluster)?;
        self.u32(fields.file_record_size)?;
        self.u32(fields.index_record_size)?;
        for value in [
            fields.total_sectors,
            fields.mft_lcn,
            fields.mftmirr_lcn,
            fields.serial,
        ] {
            self.u64(value)?;
        }
        let context = &volume.context;
        for value in [context.scanned_at_filetime, context.free_bytes] {
            self.u8(u8::from(value.is_some()))?;
            self.u64(value.unwrap_or(0))?;
        }
        self.u8(u8::from(context.label.is_some()))?;
        let label = context.label.as_deref().unwrap_or("");
        self.u32(Layout::u32(label.len(), "label length")?)?;
        Ok(())
    }

    fn stats(&mut self, stats: &ScanStats) -> Result<(), PersistError> {
        let counters: [(&str, u64); format::STATS_COUNT] = stats.counters();
        for (_, value) in counters {
            self.u64(value)?;
        }
        Ok(())
    }
}

impl Writer<'_> {
    fn node(&mut self, node: &Node) -> Result<(), PersistError> {
        for value in [
            node.size,
            node.alloc,
            node.total_size,
            node.total_alloc,
            node.mtime,
        ] {
            self.u64(value)?;
        }
        for value in [
            node.parent,
            node.child_start,
            node.child_len,
            node.file_count,
            node.name_off,
        ] {
            self.u32(value)?;
        }
        self.u16(node.name_len)?;
        self.u16(node.flags)
    }

    fn section(
        &mut self,
        data: &SnapshotStorage,
        stats: &ScanStats,
        kind: SectionKind,
        position: usize,
        len: usize,
        extension_offset: &mut usize,
    ) -> Result<(), PersistError> {
        let invalid = || PersistError::Overflow {
            operation: "column range",
        };
        match kind {
            SectionKind::Volume => self.volume_header(&data.volume)?,
            SectionKind::Stats => self.stats(stats)?,
            SectionKind::Nodes => {
                for node in data
                    .nodes
                    .get(position / 64..)
                    .ok_or_else(invalid)?
                    .iter()
                    .take(len / 64)
                {
                    self.node(node)?;
                }
            }
            SectionKind::Children | SectionKind::TopFiles => {
                let ids = if matches!(kind, SectionKind::Children) {
                    &data.children
                } else {
                    &data.top_files
                };
                for id in ids
                    .get(position / 4..)
                    .ok_or_else(invalid)?
                    .iter()
                    .take(len / 4)
                {
                    self.u32(id.raw())?;
                }
            }
            SectionKind::AdsSize => {
                for &v in data
                    .ads_size
                    .get(position / 8..)
                    .ok_or_else(invalid)?
                    .iter()
                    .take(len / 8)
                {
                    self.u64(v)?;
                }
            }
            SectionKind::Attrs => {
                for &v in data
                    .attrs
                    .get(position / 4..)
                    .ok_or_else(invalid)?
                    .iter()
                    .take(len / 4)
                {
                    self.u32(v)?;
                }
            }
            SectionKind::Nlink => {
                for &v in data
                    .nlink
                    .get(position / 2..)
                    .ok_or_else(invalid)?
                    .iter()
                    .take(len / 2)
                {
                    self.u16(v)?;
                }
            }
            SectionKind::FileRefs => {
                self.optional_chunk(&data.file_refs, position, len, 8, |writer, value| {
                    writer.u64(value.map_or(0, adamite_ntfs::record::reference::FileRef::raw))
                })?
            }
            SectionKind::ReparseTags => {
                self.optional_chunk(&data.reparse_tags, position, len, 4, |writer, value| {
                    writer.u32(value.unwrap_or(0))
                })?
            }
            SectionKind::ExtStats => {
                for group in data
                    .ext_stats
                    .get(position / 32..)
                    .ok_or_else(invalid)?
                    .iter()
                    .take(len / 32)
                {
                    self.u64(group.count)?;
                    self.u64(group.total_size)?;
                    self.u64(group.total_alloc)?;
                    self.u32(Layout::u32(*extension_offset, "extension offset")?)?;
                    self.u32(Layout::u32(group.extension.len(), "extension length")?)?;
                    *extension_offset = extension_offset.checked_add(group.extension.len()).ok_or(
                        PersistError::Overflow {
                            operation: "extension offset",
                        },
                    )?;
                }
            }
            SectionKind::Names | SectionKind::ExtNames => return Err(invalid()),
        }
        Ok(())
    }

    fn optional_chunk<T: Copy>(
        &mut self,
        values: &[Option<T>],
        position: usize,
        len: usize,
        width: usize,
        mut write: impl FnMut(&mut Self, Option<T>) -> Result<(), PersistError>,
    ) -> Result<(), PersistError> {
        let invalid = || PersistError::Overflow {
            operation: "optional column range",
        };
        let bitmap_end = Self::align_up(values.len().div_ceil(8), 8)?;
        if position < bitmap_end {
            let end = position.checked_add(len).ok_or_else(invalid)?;
            for byte in position..end {
                let start = byte.checked_mul(8).ok_or_else(invalid)?;
                let bitmap = values
                    .get(start..)
                    .unwrap_or(&[])
                    .iter()
                    .take(8)
                    .enumerate()
                    .fold(0u8, |bits, (bit, value)| {
                        bits | (u8::from(value.is_some()) << bit)
                    });
                self.u8(bitmap)?;
            }
        } else {
            let start = position
                .checked_sub(bitmap_end)
                .and_then(|v| v.checked_div(width))
                .ok_or_else(invalid)?;
            let count = len.checked_div(width).ok_or_else(invalid)?;
            for &value in values.get(start..).ok_or_else(invalid)?.iter().take(count) {
                write(self, value)?;
            }
        }
        Ok(())
    }
}
