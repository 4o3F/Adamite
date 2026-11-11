use super::{
    PersistError,
    format::{self, Crc32, SectionKind, Writer},
    validate::Validator,
};
use crate::{
    scan::{ScanContext, ScanStats},
    snapshot::{
        ExtensionStat, Node, NodeId, Snapshot, SnapshotError, VolumeInfo, storage::SnapshotStorage,
    },
};
use adamite_ntfs::{
    boot::{GeometryFields, VolumeGeometry},
    record::reference::FileRef,
};

struct Reader<'a> {
    bytes: &'a [u8],
    cursor: usize,
    section: u32,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8], section: u32) -> Self {
        Self {
            bytes,
            cursor: 0,
            section,
        }
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], PersistError> {
        let end = self.cursor.checked_add(len).ok_or(PersistError::Overflow {
            operation: "read end",
        })?;
        let value = self
            .bytes
            .get(self.cursor..end)
            .ok_or(PersistError::Truncated {
                needed: end,
                available: self.bytes.len(),
            })?;
        self.cursor = end;
        Ok(value)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], PersistError> {
        let mut value = [0; N];
        value.copy_from_slice(self.take(N)?);
        Ok(value)
    }

    fn u8(&mut self) -> Result<u8, PersistError> {
        let [value] = self.array()?;
        Ok(value)
    }

    fn u16(&mut self) -> Result<u16, PersistError> {
        self.array().map(u16::from_le_bytes)
    }

    fn u32(&mut self) -> Result<u32, PersistError> {
        self.array().map(u32::from_le_bytes)
    }

    fn u64(&mut self) -> Result<u64, PersistError> {
        self.array().map(u64::from_le_bytes)
    }

    fn host(value: u64) -> Result<usize, PersistError> {
        usize::try_from(value).map_err(|_| PersistError::Overflow {
            operation: "host length",
        })
    }

    fn invalid(&self, reason: &'static str) -> PersistError {
        PersistError::InvalidSection {
            section: self.section,
            reason,
        }
    }

    fn zeroes(&mut self, len: usize) -> Result<(), PersistError> {
        if self.take(len)?.iter().any(|&byte| byte != 0) {
            return Err(self.invalid("nonzero padding"));
        }
        Ok(())
    }

    fn tag(&mut self) -> Result<bool, PersistError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(self.invalid("presence tag is not zero or one")),
        }
    }

    fn optional_u64(&mut self) -> Result<Option<u64>, PersistError> {
        let present = self.tag()?;
        let value = self.u64()?;
        if !present && value != 0 {
            return Err(self.invalid("nonzero absent payload"));
        }
        Ok(present.then_some(value))
    }

    fn string(&mut self, len: usize) -> Result<String, PersistError> {
        let text = std::str::from_utf8(self.take(len)?).map_err(|source| PersistError::Utf8 {
            section: self.section,
            source,
        })?;
        let mut owned = String::new();
        owned.try_reserve_exact(text.len())?;
        owned.push_str(text);
        Ok(owned)
    }

    fn volume(&mut self) -> Result<VolumeInfo, PersistError> {
        let fields = GeometryFields {
            bytes_per_sector: self.u16()?,
            sectors_per_cluster: self.u32()?,
            file_record_size: self.u32()?,
            index_record_size: self.u32()?,
            total_sectors: self.u64()?,
            mft_lcn: self.u64()?,
            mftmirr_lcn: self.u64()?,
            serial: self.u64()?,
        };
        let scanned_at_filetime = self.optional_u64()?;
        let free_bytes = self.optional_u64()?;
        let present = self.tag()?;
        let len = Self::host(u64::from(self.u32()?))?;
        if !present && len != 0 {
            return Err(self.invalid("nonzero absent label length"));
        }
        let label = if present {
            Some(self.string(len)?)
        } else {
            None
        };
        let geometry = VolumeGeometry::from_fields(fields)
            .map_err(|source| PersistError::Geometry { source })?;
        Ok(VolumeInfo {
            geometry,
            context: ScanContext {
                label,
                scanned_at_filetime,
                free_bytes,
            },
        })
    }

    fn stats(&mut self) -> Result<ScanStats, PersistError> {
        Ok(ScanStats {
            records_total: self.u64()?,
            records_in_use: self.u64()?,
            base_records: self.u64()?,
            extension_records: self.u64()?,
            unused: self.u64()?,
            deleted: self.u64()?,
            baad: self.u64()?,
            corrupt_records: self.u64()?,
            bad_fixup: self.u64()?,
            invalid_fixup_metadata: self.u64()?,
            invalid_signature: self.u64()?,
            corrupt_headers: self.u64()?,
            corrupt_attributes: self.u64()?,
            type_storage_mismatches: self.u64()?,
            corrupt_values: self.u64()?,
            corrupt_runlists: self.u64()?,
            out_of_volume_extents: self.u64()?,
            invalid_attribute_lists: self.u64()?,
            invalid_file_refs: self.u64()?,
            invalid_utf16: self.u64()?,
            rejected_extensions: self.u64()?,
            corrupt_streams: self.u64()?,
            orphans: self.u64()?,
            cycle_nodes: self.u64()?,
            hardlink_duplicates: self.u64()?,
            metafiles: self.u64()?,
        })
    }

    fn node(&mut self) -> Result<Node, PersistError> {
        Ok(Node {
            size: self.u64()?,
            alloc: self.u64()?,
            total_size: self.u64()?,
            total_alloc: self.u64()?,
            mtime: self.u64()?,
            parent: self.u32()?,
            child_start: self.u32()?,
            child_len: self.u32()?,
            file_count: self.u32()?,
            name_off: self.u32()?,
            name_len: self.u16()?,
            flags: self.u16()?,
        })
    }

    fn values<T>(
        &mut self,
        count: usize,
        mut read: impl FnMut(&mut Self) -> Result<T, PersistError>,
    ) -> Result<Vec<T>, PersistError> {
        let mut values = Vec::new();
        values.try_reserve_exact(count)?;
        for _ in 0..count {
            values.push(read(self)?);
        }
        Ok(values)
    }

    fn optional<T>(
        &mut self,
        count: usize,
        mut read: impl FnMut(&mut Self) -> Result<(T, bool), PersistError>,
    ) -> Result<Vec<Option<T>>, PersistError> {
        let bitmap_len = count.checked_add(7).ok_or(PersistError::Overflow {
            operation: "bitmap length",
        })? / 8;
        let bitmap = self.take(bitmap_len)?;
        if !count.is_multiple_of(8) && bitmap.last().is_some_and(|byte| byte >> (count % 8) != 0) {
            return Err(self.invalid("nonzero unused bitmap bits"));
        }
        let padding = Writer::align_up(bitmap_len, 8)?
            .checked_sub(bitmap_len)
            .ok_or(PersistError::Overflow {
                operation: "bitmap padding",
            })?;
        self.zeroes(padding)?;
        let mut values = Vec::new();
        values.try_reserve_exact(count)?;
        for index in 0..count {
            let byte = bitmap
                .get(index / 8)
                .ok_or_else(|| self.invalid("missing presence bit"))?;
            let present = byte & (1 << (index % 8)) != 0;
            let (value, zero) = read(self)?;
            if !present && !zero {
                return Err(self.invalid("nonzero absent column value"));
            }
            values.push(present.then_some(value));
        }
        Ok(values)
    }

    fn extensions(
        &mut self,
        count: usize,
        pool: &'a [u8],
    ) -> Result<Vec<ExtensionStat>, PersistError> {
        let mut names = Self::new(pool, SectionKind::ExtNames as u32);
        let groups = self.values(count, |reader| {
            let count = reader.u64()?;
            let total_size = reader.u64()?;
            let total_alloc = reader.u64()?;
            let offset = Self::host(u64::from(reader.u32()?))?;
            let len = Self::host(u64::from(reader.u32()?))?;
            if offset != names.cursor {
                return Err(reader.invalid("extension names are not contiguous"));
            }
            let extension = names.string(len)?;
            Ok(ExtensionStat {
                extension,
                count,
                total_size,
                total_alloc,
            })
        })?;
        if names.cursor != pool.len() {
            return Err(self.invalid("extension names do not cover pool"));
        }
        Ok(groups)
    }
}

#[derive(Clone, Copy, Default)]
struct Section {
    kind: u32,
    offset: usize,
    len: usize,
    count: usize,
}

impl Section {
    fn invalid(self, reason: &'static str) -> PersistError {
        PersistError::InvalidSection {
            section: self.kind,
            reason,
        }
    }

    fn end(self) -> Result<usize, PersistError> {
        self.offset
            .checked_add(self.len)
            .ok_or(PersistError::Overflow {
                operation: "section end",
            })
    }

    fn reader(self, bytes: &[u8]) -> Result<Reader<'_>, PersistError> {
        let end = self.end()?;
        let bytes = bytes.get(self.offset..end).ok_or(PersistError::Truncated {
            needed: end,
            available: bytes.len(),
        })?;
        Ok(Reader::new(bytes, self.kind))
    }

    fn validate_length(self, bytes: &[u8]) -> Result<(), PersistError> {
        let mut reader = self.reader(bytes)?;
        let width = match self.kind {
            1 => {
                if self.count != 1 || self.len < format::VOLUME_FIXED_LEN {
                    return Err(self.invalid("invalid volume count or length"));
                }
                reader.take(65)?;
                let label_len = Reader::host(u64::from(reader.u32()?))?;
                let expected = format::VOLUME_FIXED_LEN.checked_add(label_len).ok_or(
                    PersistError::Overflow {
                        operation: "volume length",
                    },
                )?;
                if self.len != expected {
                    return Err(self.invalid("volume length does not match label"));
                }
                return Ok(());
            }
            2 => {
                if self.count != format::STATS_COUNT || self.len != format::STATS_COUNT * 8 {
                    return Err(self.invalid("invalid stats count or length"));
                }
                return Ok(());
            }
            3 => 64,
            4 | 13 => 1,
            5 | 8 | 10 | 11 => 4,
            6 | 7 => 8,
            9 => 2,
            12 => 32,
            _ => return Err(self.invalid("unknown section kind")),
        };
        let mut expected = self
            .count
            .checked_mul(width)
            .ok_or(PersistError::Overflow {
                operation: "section length",
            })?;
        if self.kind == SectionKind::FileRefs as u32 || self.kind == SectionKind::ReparseTags as u32
        {
            let bitmap_len = self.count.checked_add(7).ok_or(PersistError::Overflow {
                operation: "bitmap length",
            })? / 8;
            expected = expected
                .checked_add(Writer::align_up(bitmap_len, 8)?)
                .ok_or(PersistError::Overflow {
                    operation: "optional section length",
                })?;
        }
        if self.len != expected {
            return Err(self.invalid("section length does not match count"));
        }
        Ok(())
    }
}

struct Decoder<'a> {
    bytes: &'a [u8],
    sections: [Section; format::SECTION_COUNT],
}

impl<'a> Decoder<'a> {
    fn new(bytes: &'a [u8]) -> Result<Self, PersistError> {
        if bytes.get(..format::MAGIC.len()) != Some(format::MAGIC.as_slice()) {
            return Err(PersistError::BadMagic);
        }
        if bytes.len() < format::HEADER_LEN {
            return Err(PersistError::Truncated {
                needed: format::HEADER_LEN,
                available: bytes.len(),
            });
        }
        let mut header = Reader::new(bytes, 0);
        header.take(format::MAGIC.len())?;
        let version = header.u32()?;
        if version != format::VERSION {
            return Err(PersistError::UnsupportedVersion { version });
        }
        if Reader::host(u64::from(header.u32()?))? != format::HEADER_LEN {
            return Err(PersistError::InvalidHeader {
                reason: "header length is not 64",
            });
        }
        let file_len = Reader::host(header.u64()?)?;
        if Reader::host(u64::from(header.u32()?))? != format::SECTION_COUNT {
            return Err(PersistError::InvalidHeader {
                reason: "section count is not 13",
            });
        }
        if header.u32()? != 0 {
            return Err(PersistError::InvalidHeader {
                reason: "nonzero header flags",
            });
        }
        let expected = header.u32()?;
        if header.take(28)?.iter().any(|&byte| byte != 0) {
            return Err(PersistError::InvalidHeader {
                reason: "nonzero reserved header bytes",
            });
        }
        if file_len > bytes.len() {
            return Err(PersistError::Truncated {
                needed: file_len,
                available: bytes.len(),
            });
        }
        if file_len < bytes.len() {
            return Err(PersistError::InvalidHeader {
                reason: "trailing bytes after file length",
            });
        }
        let mut sections = [Section::default(); format::SECTION_COUNT];
        let mut previous_end = format::TABLE_END;
        for (index, section) in sections.iter_mut().enumerate() {
            let kind = header.u32()?;
            let expected_kind = u32::try_from(index)
                .ok()
                .and_then(|value| value.checked_add(1))
                .ok_or(PersistError::Overflow {
                    operation: "section kind",
                })?;
            if kind != expected_kind {
                return Err(PersistError::InvalidSectionTable {
                    reason: "section kinds are not 1 through 13 in order",
                });
            }
            if header.u32()? != 0 {
                return Err(PersistError::InvalidSectionTable {
                    reason: "nonzero section flags",
                });
            }
            *section = Section {
                kind,
                offset: Reader::host(header.u64()?)?,
                len: Reader::host(header.u64()?)?,
                count: Reader::host(header.u64()?)?,
            };
            if section.offset != Writer::align_up(previous_end, 8)? {
                return Err(PersistError::InvalidSectionTable {
                    reason: "noncanonical section placement",
                });
            }
            let padding =
                bytes
                    .get(previous_end..section.offset)
                    .ok_or(PersistError::Truncated {
                        needed: section.offset,
                        available: bytes.len(),
                    })?;
            if padding.iter().any(|&byte| byte != 0) {
                return Err(PersistError::InvalidSectionTable {
                    reason: "nonzero section padding",
                });
            }
            section.validate_length(bytes)?;
            previous_end = section.end()?;
        }
        if previous_end != file_len {
            return Err(PersistError::InvalidSectionTable {
                reason: "last section does not end at file length",
            });
        }
        let actual = Crc32::checksum(bytes)?;
        if actual != expected {
            return Err(PersistError::ChecksumMismatch { expected, actual });
        }
        Ok(Self { bytes, sections })
    }

    fn section(&self, kind: SectionKind) -> Result<Section, PersistError> {
        let index = usize::try_from(kind as u32)
            .ok()
            .and_then(|value| value.checked_sub(1))
            .ok_or(PersistError::Overflow {
                operation: "section index",
            })?;
        self.sections
            .get(index)
            .copied()
            .ok_or(PersistError::InvalidSectionTable {
                reason: "missing section",
            })
    }

    fn reader(&self, kind: SectionKind) -> Result<Reader<'a>, PersistError> {
        self.section(kind)?.reader(self.bytes)
    }

    fn values<T>(
        &self,
        kind: SectionKind,
        read: impl FnMut(&mut Reader<'a>) -> Result<T, PersistError>,
    ) -> Result<Vec<T>, PersistError> {
        self.reader(kind)?.values(self.section(kind)?.count, read)
    }

    fn decode(self) -> Result<(Snapshot, ScanStats), PersistError> {
        let volume = self.reader(SectionKind::Volume)?.volume()?;
        let stats = self.reader(SectionKind::Stats)?.stats()?;
        let nodes = self.values(SectionKind::Nodes, Reader::node)?;
        let names = self.values(SectionKind::Names, Reader::u8)?;
        let children = self.values(SectionKind::Children, |reader| {
            reader.u32().map(NodeId::from_raw)
        })?;
        let ads_size = self.values(SectionKind::AdsSize, Reader::u64)?;
        let file_refs = self.reader(SectionKind::FileRefs)?.optional(
            self.section(SectionKind::FileRefs)?.count,
            |reader| {
                let value = reader.u64()?;
                Ok((FileRef::from_raw(value), value == 0))
            },
        )?;
        let attrs = self.values(SectionKind::Attrs, Reader::u32)?;
        let nlink = self.values(SectionKind::Nlink, Reader::u16)?;
        let reparse_tags = self.reader(SectionKind::ReparseTags)?.optional(
            self.section(SectionKind::ReparseTags)?.count,
            |reader| {
                let value = reader.u32()?;
                Ok((value, value == 0))
            },
        )?;
        let top_files = self.values(SectionKind::TopFiles, |reader| {
            reader.u32().map(NodeId::from_raw)
        })?;
        let ext_stats = self.reader(SectionKind::ExtStats)?.extensions(
            self.section(SectionKind::ExtStats)?.count,
            self.reader(SectionKind::ExtNames)?.bytes,
        )?;
        let data = SnapshotStorage {
            nodes,
            names,
            children,
            ads_size,
            file_refs,
            attrs,
            nlink,
            reparse_tags,
            volume,
            top_files,
            ext_stats,
        };
        Validator::new(&data).validate()?;
        Ok((
            Snapshot::from_storage(data).map_err(PersistError::from_snapshot)?,
            stats,
        ))
    }
}

impl PersistError {
    pub(super) fn from_snapshot(error: SnapshotError) -> Self {
        match error {
            SnapshotError::Allocation { source } => Self::Allocation { source },
            SnapshotError::Overflow { field } => Self::Overflow { operation: field },
            SnapshotError::InvalidTree { reason } => Self::InvalidTree { reason, node: 0 },
        }
    }
}

impl Snapshot {
    pub(crate) fn from_adm_bytes(bytes: &[u8]) -> Result<(Self, ScanStats), PersistError> {
        Decoder::new(bytes)?.decode()
    }
}

#[cfg(test)]
mod tests {
    use super::Reader;
    use std::error::Error;

    #[test]
    fn decoded_stats_follow_public_counter_order() -> Result<(), Box<dyn Error>> {
        let bytes: Vec<_> = (1u64..=26).flat_map(u64::to_le_bytes).collect();
        let stats = Reader::new(&bytes, 2).stats()?;
        for ((name, value), expected) in stats.counters().into_iter().zip(1..=26) {
            assert_eq!(value, expected, "counter {name}");
        }
        Ok(())
    }
}
