//! Checked layout calculation followed by one fallible output allocation.

use super::{
    PersistError,
    format::{self, Crc32, SectionKind, Writer},
};
use crate::{
    scan::ScanStats,
    snapshot::{Snapshot, VolumeInfo, storage::SnapshotStorage},
};

#[derive(Clone, Copy)]
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

impl Snapshot {
    pub(crate) fn to_adm_bytes(&self, stats: &ScanStats) -> Result<Vec<u8>, PersistError> {
        let data = self.data();
        let layout = Layout::new(data)?;
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(layout.file_len)?;
        bytes.resize(layout.file_len, 0);
        layout.write_header(&mut Writer::new(&mut bytes))?;
        for section in layout.sections {
            let end = section.end()?;
            let target = bytes
                .get_mut(section.offset..end)
                .ok_or(PersistError::Overflow {
                    operation: "section write range",
                })?;
            let mut writer = Writer::new(target);
            match section.kind {
                SectionKind::Volume => writer.volume(&data.volume)?,
                SectionKind::Stats => writer.stats(stats)?,
                SectionKind::Nodes => {
                    for node in &data.nodes {
                        for value in [
                            node.size,
                            node.alloc,
                            node.total_size,
                            node.total_alloc,
                            node.mtime,
                        ] {
                            writer.u64(value)?;
                        }
                        for value in [
                            node.parent,
                            node.child_start,
                            node.child_len,
                            node.file_count,
                            node.name_off,
                        ] {
                            writer.u32(value)?;
                        }
                        writer.u16(node.name_len)?;
                        writer.u16(node.flags)?;
                    }
                }
                SectionKind::Names => writer.bytes(&data.names)?,
                SectionKind::Children | SectionKind::TopFiles => {
                    let ids = if matches!(section.kind, SectionKind::Children) {
                        &data.children
                    } else {
                        &data.top_files
                    };
                    for id in ids {
                        writer.u32(id.raw())?;
                    }
                }
                SectionKind::AdsSize => {
                    for &value in &data.ads_size {
                        writer.u64(value)?;
                    }
                }
                SectionKind::FileRefs => writer.optional(&data.file_refs, |writer, value| {
                    writer.u64(value.map_or(0, adamite_ntfs::record::reference::FileRef::raw))
                })?,
                SectionKind::Attrs => {
                    for &value in &data.attrs {
                        writer.u32(value)?;
                    }
                }
                SectionKind::Nlink => {
                    for &value in &data.nlink {
                        writer.u16(value)?;
                    }
                }
                SectionKind::ReparseTags => writer
                    .optional(&data.reparse_tags, |writer, value| {
                        writer.u32(value.unwrap_or(0))
                    })?,
                SectionKind::ExtStats => {
                    let mut offset = 0usize;
                    for group in &data.ext_stats {
                        writer.u64(group.count)?;
                        writer.u64(group.total_size)?;
                        writer.u64(group.total_alloc)?;
                        writer.u32(Layout::u32(offset, "extension offset")?)?;
                        writer.u32(Layout::u32(group.extension.len(), "extension length")?)?;
                        offset = offset.checked_add(group.extension.len()).ok_or(
                            PersistError::Overflow {
                                operation: "extension offset",
                            },
                        )?;
                    }
                }
                SectionKind::ExtNames => {
                    for group in &data.ext_stats {
                        writer.bytes(group.extension.as_bytes())?;
                    }
                }
            }
        }
        let checksum = Crc32::checksum(&bytes)?;
        let target = bytes.get_mut(32..36).ok_or(PersistError::Overflow {
            operation: "checksum write range",
        })?;
        Writer::new(target).u32(checksum)?;
        Ok(bytes)
    }
}

impl Writer<'_> {
    fn volume(&mut self, volume: &VolumeInfo) -> Result<(), PersistError> {
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
        self.bytes(label.as_bytes())
    }

    fn stats(&mut self, stats: &ScanStats) -> Result<(), PersistError> {
        let counters: [u64; format::STATS_COUNT] = [
            stats.records_total,
            stats.records_in_use,
            stats.base_records,
            stats.extension_records,
            stats.unused,
            stats.deleted,
            stats.baad,
            stats.corrupt_records,
            stats.bad_fixup,
            stats.invalid_fixup_metadata,
            stats.invalid_signature,
            stats.corrupt_headers,
            stats.corrupt_attributes,
            stats.type_storage_mismatches,
            stats.corrupt_values,
            stats.corrupt_runlists,
            stats.out_of_volume_extents,
            stats.invalid_attribute_lists,
            stats.invalid_file_refs,
            stats.invalid_utf16,
            stats.rejected_extensions,
            stats.corrupt_streams,
            stats.orphans,
            stats.cycle_nodes,
            stats.hardlink_duplicates,
            stats.metafiles,
        ];
        for value in counters {
            self.u64(value)?;
        }
        Ok(())
    }
}
