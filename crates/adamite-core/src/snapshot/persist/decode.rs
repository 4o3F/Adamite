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

struct Reader<R, E> {
    read: R,
    available: usize,
    error: std::marker::PhantomData<E>,
    cursor: usize,
    section: u32,
}

impl<R, E> Reader<R, E>
where
    R: FnMut(&mut [u8]) -> Result<(), E>,
    E: From<PersistError>,
{
    fn new(read: R, available: usize, section: u32) -> Self {
        Self {
            read,
            available,
            error: std::marker::PhantomData,
            cursor: 0,
            section,
        }
    }

    fn read_exact(&mut self, bytes: &mut [u8]) -> Result<(), E> {
        let end = self
            .cursor
            .checked_add(bytes.len())
            .ok_or(PersistError::Overflow {
                operation: "read end",
            })?;
        if end > self.available {
            return Err(PersistError::Truncated {
                needed: end,
                available: self.available,
            }
            .into());
        }
        (self.read)(bytes)?;
        self.cursor = end;
        Ok(())
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], E> {
        let mut value = [0; N];
        self.read_exact(&mut value)?;
        Ok(value)
    }

    fn skip(&mut self, len: usize) -> Result<(), E> {
        for _ in 0..len {
            self.u8()?;
        }
        Ok(())
    }

    fn u8(&mut self) -> Result<u8, E> {
        let [value] = self.array()?;
        Ok(value)
    }

    fn u16(&mut self) -> Result<u16, E> {
        self.array().map(u16::from_le_bytes)
    }

    fn u32(&mut self) -> Result<u32, E> {
        self.array().map(u32::from_le_bytes)
    }

    fn u64(&mut self) -> Result<u64, E> {
        self.array().map(u64::from_le_bytes)
    }

    fn invalid(&self, reason: &'static str) -> PersistError {
        PersistError::InvalidSection {
            section: self.section,
            reason,
        }
    }

    fn zeroes(&mut self, len: usize) -> Result<(), E> {
        if (0..len).try_fold(false, |nonzero, _| {
            self.u8().map(|byte| nonzero || byte != 0)
        })? {
            return Err(self.invalid("nonzero padding").into());
        }
        Ok(())
    }

    fn tag(&mut self) -> Result<bool, E> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(self.invalid("presence tag is not zero or one").into()),
        }
    }

    fn optional_u64(&mut self) -> Result<Option<u64>, E> {
        let present = self.tag()?;
        let value = self.u64()?;
        if !present && value != 0 {
            return Err(self.invalid("nonzero absent payload").into());
        }
        Ok(present.then_some(value))
    }

    fn string(&mut self, len: usize) -> Result<String, E> {
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(len).map_err(PersistError::from)?;
        bytes.resize(len, 0);
        self.read_exact(&mut bytes)?;
        String::from_utf8(bytes).map_err(|error| {
            PersistError::Utf8 {
                section: self.section,
                source: error.utf8_error(),
            }
            .into()
        })
    }

    fn volume(&mut self) -> Result<VolumeInfo, E> {
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
        let len = Length::host(u64::from(self.u32()?))?;
        if !present && len != 0 {
            return Err(self.invalid("nonzero absent label length").into());
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

    fn stats(&mut self) -> Result<ScanStats, E> {
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

    fn node(&mut self) -> Result<Node, E> {
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
        mut read: impl FnMut(&mut Self) -> Result<T, E>,
    ) -> Result<Vec<T>, E> {
        let mut values = Vec::new();
        values
            .try_reserve_exact(count)
            .map_err(PersistError::from)?;
        for _ in 0..count {
            values.push(read(self)?);
        }
        Ok(values)
    }

    fn optional<T: Copy>(
        &mut self,
        count: usize,
        marker: T,
        mut read: impl FnMut(&mut Self) -> Result<(T, bool), E>,
    ) -> Result<Vec<Option<T>>, E> {
        let bitmap_len = count.checked_add(7).ok_or(PersistError::Overflow {
            operation: "bitmap length",
        })? / 8;
        let mut values = Vec::new();
        values
            .try_reserve_exact(count)
            .map_err(PersistError::from)?;
        // Stage presence in the final column until its value array arrives.
        for _ in 0..bitmap_len {
            let byte = self.u8()?;
            let remaining = count
                .checked_sub(values.len())
                .ok_or(PersistError::Overflow {
                    operation: "bitmap remaining",
                })?;
            if remaining < 8 && byte >> remaining != 0 {
                return Err(self.invalid("nonzero unused bitmap bits").into());
            }
            for bit in 0..remaining.min(8) {
                values.push((byte & (1 << bit) != 0).then_some(marker));
            }
        }
        let padding = Writer::align_up(bitmap_len, 8)?
            .checked_sub(bitmap_len)
            .ok_or(PersistError::Overflow {
                operation: "bitmap padding",
            })?;
        self.zeroes(padding)?;
        for slot in &mut values {
            let present = slot.is_some();
            let (value, zero) = read(self)?;
            if !present && !zero {
                return Err(self.invalid("nonzero absent column value").into());
            }
            *slot = present.then_some(value);
        }
        Ok(values)
    }
}

struct Length;
impl Length {
    fn host(value: u64) -> Result<usize, PersistError> {
        usize::try_from(value).map_err(|_| PersistError::Overflow {
            operation: "host length",
        })
    }
}

#[derive(Debug, Clone, Copy, Default)]
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

    fn validate_length(self, volume: &[u8], file_len: usize) -> Result<(), PersistError> {
        let end = self.end()?;
        if end > file_len {
            return Err(PersistError::Truncated {
                needed: end,
                available: file_len,
            });
        }
        let mut slice = volume;
        let mut reader = Reader::new(
            |target: &mut [u8]| -> Result<(), PersistError> {
                let chunk = slice.get(..target.len()).ok_or(PersistError::Overflow {
                    operation: "volume prefix",
                })?;
                target.copy_from_slice(chunk);
                slice = slice.get(target.len()..).ok_or(PersistError::Overflow {
                    operation: "volume prefix",
                })?;
                Ok(())
            },
            volume.len(),
            self.kind,
        );
        let width = match self.kind {
            1 => {
                if self.count != 1 || self.len < format::VOLUME_FIXED_LEN {
                    return Err(self.invalid("invalid volume count or length"));
                }
                reader.skip(65)?;
                let label_len = Length::host(u64::from(reader.u32()?))?;
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

/// Validated file layout for a second sequential pass over the same bytes.
#[derive(Debug)]
pub struct SnapshotDecoder {
    file_len: usize,
    sections: [Section; format::SECTION_COUNT],
}

impl SnapshotDecoder {
    /// Verifies the header, table, padding and whole-file CRC without allocating columns.
    ///
    /// `read` supplies consecutive bytes from the start of a file of `file_len` bytes.
    /// The caller must provide the same immutable bytes for the decoding pass.
    ///
    /// # Errors
    /// Returns the existing format errors or an error supplied by the caller.
    pub fn verify<E: From<PersistError>>(
        file_len: usize,
        mut read: impl FnMut(&mut [u8]) -> Result<(), E>,
    ) -> Result<Self, E> {
        let mut metadata = [0u8; format::TABLE_END];
        let available = file_len.min(metadata.len());
        let bytes = metadata
            .get_mut(..available)
            .ok_or(PersistError::Overflow {
                operation: "metadata range",
            })?;
        read(bytes)?;
        if bytes.get(..format::MAGIC.len()) != Some(format::MAGIC.as_slice()) {
            return Err(PersistError::BadMagic.into());
        }
        if bytes.len() < format::HEADER_LEN {
            return Err(PersistError::Truncated {
                needed: format::HEADER_LEN,
                available: file_len,
            }
            .into());
        }
        let mut input = SliceInput::new(bytes);
        let mut header = Reader::new(|target| input.read(target), available, 0);
        header.skip(format::MAGIC.len())?;
        let version = header.u32()?;
        if version != format::VERSION {
            return Err(PersistError::UnsupportedVersion { version }.into());
        }
        if Length::host(u64::from(header.u32()?))? != format::HEADER_LEN {
            return Err(PersistError::InvalidHeader {
                reason: "header length is not 64",
            }
            .into());
        }
        let declared_len = Length::host(header.u64()?)?;
        if Length::host(u64::from(header.u32()?))? != format::SECTION_COUNT {
            return Err(PersistError::InvalidHeader {
                reason: "section count is not 13",
            }
            .into());
        }
        if header.u32()? != 0 {
            return Err(PersistError::InvalidHeader {
                reason: "nonzero header flags",
            }
            .into());
        }
        let expected = header.u32()?;
        if header.array::<28>()?.iter().any(|&byte| byte != 0) {
            return Err(PersistError::InvalidHeader {
                reason: "nonzero reserved header bytes",
            }
            .into());
        }
        if declared_len > file_len {
            return Err(PersistError::Truncated {
                needed: declared_len,
                available: file_len,
            }
            .into());
        }
        if declared_len < file_len {
            return Err(PersistError::InvalidHeader {
                reason: "trailing bytes after file length",
            }
            .into());
        }
        let mut sections = [Section::default(); format::SECTION_COUNT];
        let mut previous_end = format::TABLE_END;
        let mut crc = Crc32::new();
        crc.update(bytes)?;
        let mut scratch = Vec::new();
        scratch
            .try_reserve_exact(1 << 20)
            .map_err(PersistError::from)?;
        scratch.resize(1 << 20, 0);
        let mut position = available;
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
                }
                .into());
            }
            if header.u32()? != 0 {
                return Err(PersistError::InvalidSectionTable {
                    reason: "nonzero section flags",
                }
                .into());
            }
            *section = Section {
                kind,
                offset: Length::host(header.u64()?)?,
                len: Length::host(header.u64()?)?,
                count: Length::host(header.u64()?)?,
            };
            if section.offset != Writer::align_up(previous_end, 8)? {
                return Err(PersistError::InvalidSectionTable {
                    reason: "noncanonical section placement",
                }
                .into());
            }
            let padding_len =
                section
                    .offset
                    .checked_sub(previous_end)
                    .ok_or(PersistError::Overflow {
                        operation: "section padding",
                    })?;
            if section.offset > file_len {
                return Err(PersistError::Truncated {
                    needed: section.offset,
                    available: file_len,
                }
                .into());
            }
            let padding = scratch
                .get_mut(..padding_len)
                .ok_or(PersistError::Overflow {
                    operation: "padding buffer",
                })?;
            read(padding)?;
            crc.update(padding)?;
            position = position
                .checked_add(padding_len)
                .ok_or(PersistError::Overflow {
                    operation: "stream position",
                })?;
            if padding.iter().any(|&byte| byte != 0) {
                return Err(PersistError::InvalidSectionTable {
                    reason: "nonzero section padding",
                }
                .into());
            }
            let end = section.end()?;
            if end > file_len {
                return Err(PersistError::Truncated {
                    needed: end,
                    available: file_len,
                }
                .into());
            }
            let mut volume = [0u8; format::VOLUME_FIXED_LEN];
            if section.kind == 1 && section.len >= format::VOLUME_FIXED_LEN && section.count == 1 {
                read(&mut volume)?;
                crc.update(&volume)?;
                position = position
                    .checked_add(volume.len())
                    .ok_or(PersistError::Overflow {
                        operation: "stream position",
                    })?;
            }
            section.validate_length(&volume, file_len)?;
            while position < end {
                let len = end
                    .checked_sub(position)
                    .ok_or(PersistError::Overflow {
                        operation: "stream remaining",
                    })?
                    .min(scratch.len());
                let chunk = scratch.get_mut(..len).ok_or(PersistError::Overflow {
                    operation: "verification buffer",
                })?;
                read(chunk)?;
                crc.update(chunk)?;
                position = position.checked_add(len).ok_or(PersistError::Overflow {
                    operation: "stream position",
                })?;
            }
            previous_end = section.end()?;
        }
        if previous_end != file_len {
            return Err(PersistError::InvalidSectionTable {
                reason: "last section does not end at file length",
            }
            .into());
        }
        let actual = crc.finish();
        if actual != expected {
            return Err(PersistError::ChecksumMismatch { expected, actual }.into());
        }
        Ok(Self { file_len, sections })
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

    fn reader<'a, R, E>(
        &self,
        read: &'a mut R,
        position: &mut usize,
        kind: SectionKind,
    ) -> Result<Reader<&'a mut R, E>, E>
    where
        R: FnMut(&mut [u8]) -> Result<(), E>,
        E: From<PersistError>,
    {
        let section = self.section(kind)?;
        let padding = section
            .offset
            .checked_sub(*position)
            .ok_or(PersistError::Overflow {
                operation: "section padding",
            })?;
        for _ in 0..padding {
            read(&mut [0u8; 1])?;
        }
        *position = section.end()?;
        Ok(Reader::new(read, section.len, section.kind))
    }

    /// Decodes a second sequential pass into final column storage and validates its invariants.
    ///
    /// # Errors
    /// Returns value, allocation and snapshot validation errors, or caller read errors.
    pub fn decode<E: From<PersistError>>(
        self,
        mut read: impl FnMut(&mut [u8]) -> Result<(), E>,
    ) -> Result<(Snapshot, ScanStats), E> {
        let mut input = Reader::new(&mut read, self.file_len, 0);
        input.skip(format::TABLE_END)?;
        let mut position = format::TABLE_END;
        let volume = self
            .reader(&mut read, &mut position, SectionKind::Volume)?
            .volume()?;
        let stats = self
            .reader(&mut read, &mut position, SectionKind::Stats)?
            .stats()?;
        let nodes = self
            .reader(&mut read, &mut position, SectionKind::Nodes)?
            .values(self.section(SectionKind::Nodes)?.count, Reader::node)?;
        let names = self
            .reader(&mut read, &mut position, SectionKind::Names)?
            .values(self.section(SectionKind::Names)?.count, Reader::u8)?;
        let children = self
            .reader(&mut read, &mut position, SectionKind::Children)?
            .values(self.section(SectionKind::Children)?.count, |r| {
                r.u32().map(NodeId::from_raw)
            })?;
        let ads_size = self
            .reader(&mut read, &mut position, SectionKind::AdsSize)?
            .values(self.section(SectionKind::AdsSize)?.count, Reader::u64)?;
        let file_refs = self
            .reader(&mut read, &mut position, SectionKind::FileRefs)?
            .optional(
                self.section(SectionKind::FileRefs)?.count,
                FileRef::from_raw(0),
                |r| {
                    let value = r.u64()?;
                    Ok((FileRef::from_raw(value), value == 0))
                },
            )?;
        let attrs = self
            .reader(&mut read, &mut position, SectionKind::Attrs)?
            .values(self.section(SectionKind::Attrs)?.count, Reader::u32)?;
        let nlink = self
            .reader(&mut read, &mut position, SectionKind::Nlink)?
            .values(self.section(SectionKind::Nlink)?.count, Reader::u16)?;
        let reparse_tags = self
            .reader(&mut read, &mut position, SectionKind::ReparseTags)?
            .optional(self.section(SectionKind::ReparseTags)?.count, 0, |r| {
                let value = r.u32()?;
                Ok((value, value == 0))
            })?;
        let top_files = self
            .reader(&mut read, &mut position, SectionKind::TopFiles)?
            .values(self.section(SectionKind::TopFiles)?.count, |r| {
                r.u32().map(NodeId::from_raw)
            })?;
        let mut extension_offset = 0usize;
        // Report name-table errors in group order, interleaved with UTF-8 validation.
        let mut extension_error = None;
        let pool_len = self.section(SectionKind::ExtNames)?.len;
        let mut ext_stats = self
            .reader(&mut read, &mut position, SectionKind::ExtStats)?
            .values(self.section(SectionKind::ExtStats)?.count, |r| {
                let count = r.u64()?;
                let total_size = r.u64()?;
                let total_alloc = r.u64()?;
                let offset = Length::host(u64::from(r.u32()?))?;
                let len = Length::host(u64::from(r.u32()?))?;
                if extension_error.is_none() && offset != extension_offset {
                    extension_error = Some((
                        extension_offset,
                        r.invalid("extension names are not contiguous"),
                    ));
                }
                let end = match extension_offset.checked_add(len) {
                    Some(end) => end,
                    None => {
                        if extension_error.is_none() {
                            extension_error = Some((
                                extension_offset,
                                PersistError::Overflow {
                                    operation: "read end",
                                },
                            ));
                        }
                        extension_offset
                    }
                };
                if extension_error.is_none() && end > pool_len {
                    extension_error = Some((
                        extension_offset,
                        PersistError::Truncated {
                            needed: end,
                            available: pool_len,
                        },
                    ));
                }
                extension_offset = end;
                let mut extension = String::new();
                if extension_error.is_none() {
                    extension
                        .try_reserve_exact(len)
                        .map_err(PersistError::from)?;
                    for _ in 0..len {
                        extension.push('\0');
                    }
                }
                Ok(ExtensionStat {
                    extension,
                    count,
                    total_size,
                    total_alloc,
                })
            })?;
        let mut pool = self.reader(&mut read, &mut position, SectionKind::ExtNames)?;
        for group in &mut ext_stats {
            if extension_error
                .as_ref()
                .is_some_and(|(offset, _)| *offset == pool.cursor)
                && let Some((_, error)) = extension_error.take()
            {
                return Err(error.into());
            }
            let mut bytes = std::mem::take(&mut group.extension).into_bytes();
            pool.read_exact(&mut bytes)?;
            group.extension = String::from_utf8(bytes).map_err(|error| PersistError::Utf8 {
                section: SectionKind::ExtNames as u32,
                source: error.utf8_error(),
            })?;
        }
        if pool.cursor != pool_len {
            return Err(PersistError::InvalidSection {
                section: SectionKind::ExtStats as u32,
                reason: "extension names do not cover pool",
            }
            .into());
        }
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

pub(super) struct SliceInput<'a> {
    bytes: &'a [u8],
}
impl<'a> SliceInput<'a> {
    pub(super) fn new(bytes: &'a [u8]) -> Self {
        Self { bytes }
    }
    pub(super) fn read(&mut self, target: &mut [u8]) -> Result<(), PersistError> {
        let bytes = self
            .bytes
            .get(..target.len())
            .ok_or(PersistError::Truncated {
                needed: target.len(),
                available: self.bytes.len(),
            })?;
        target.copy_from_slice(bytes);
        self.bytes = self
            .bytes
            .get(target.len()..)
            .ok_or(PersistError::Overflow {
                operation: "slice input",
            })?;
        Ok(())
    }
}

impl Snapshot {
    pub(crate) fn from_adm_bytes(bytes: &[u8]) -> Result<(Self, ScanStats), PersistError> {
        let mut first = SliceInput::new(bytes);
        let decoder = SnapshotDecoder::verify(bytes.len(), |target| first.read(target))?;
        let mut second = SliceInput::new(bytes);
        decoder.decode(|target| second.read(target))
    }
}

#[cfg(test)]
mod tests {
    use super::Reader;
    use std::error::Error;

    #[test]
    fn decoded_stats_follow_public_counter_order() -> Result<(), Box<dyn Error>> {
        let bytes: Vec<_> = (1u64..=26).flat_map(u64::to_le_bytes).collect();
        let mut input = super::SliceInput::new(&bytes);
        let stats = Reader::new(|target| input.read(target), bytes.len(), 2).stats()?;
        for ((name, value), expected) in stats.counters().into_iter().zip(1..=26) {
            assert_eq!(value, expected, "counter {name}");
        }
        Ok(())
    }
}
