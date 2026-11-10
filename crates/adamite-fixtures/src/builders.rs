//! Independent NTFS on-disk encoders for parser, core and fuzz tests.

#[derive(Debug, Clone)]
/// Configurable NTFS boot-sector encoder independent of the parser.
pub struct BootSectorBuilder {
    /// Bytes per logical sector.
    pub sector: u16,
    /// Raw sectors-per-cluster byte.
    pub cluster_encoding: u8,
    /// Signed file-record-size encoding.
    pub file_encoding: i8,
    /// Signed index-record-size encoding.
    pub index_encoding: i8,
    /// Total volume sectors.
    pub sectors: u64,
    /// MFT starting physical cluster.
    pub mft: u64,
    /// MFT mirror starting physical cluster.
    pub mirror: u64,
    /// Volume serial number.
    pub serial: u64,
}

impl Default for BootSectorBuilder {
    fn default() -> Self {
        Self {
            sector: 512,
            cluster_encoding: 8,
            file_encoding: -10,
            index_encoding: -12,
            sectors: 1 << 24,
            mft: 4,
            mirror: 8,
            serial: 0x0123_4567_89ab_cdef,
        }
    }
}

impl BootSectorBuilder {
    /// Encodes the configured fields into owned bytes.
    ///
    /// # Panics
    ///
    /// Panics if the configured lengths, offsets or values cannot fit the encoded format.
    #[must_use]
    pub fn build(&self) -> [u8; 512] {
        let mut bytes = [0; 512];
        bytes[..3].copy_from_slice(&[0xeb, 0x52, 0x90]);
        bytes[3..11].copy_from_slice(b"NTFS    ");
        bytes[0x0b..0x0d].copy_from_slice(&self.sector.to_le_bytes());
        bytes[0x0d] = self.cluster_encoding;
        bytes[0x28..0x30].copy_from_slice(&self.sectors.to_le_bytes());
        bytes[0x30..0x38].copy_from_slice(&self.mft.to_le_bytes());
        bytes[0x38..0x40].copy_from_slice(&self.mirror.to_le_bytes());
        bytes[0x40] = self.file_encoding as u8;
        bytes[0x44] = self.index_encoding as u8;
        bytes[0x48..0x50].copy_from_slice(&self.serial.to_le_bytes());
        bytes[510..].copy_from_slice(&[0x55, 0xaa]);
        bytes
    }
}

#[derive(Debug, Clone)]
/// FILE record encoder with configurable headers, attributes and fixup trailers.
pub struct RecordBuilder {
    /// Record buffer size in bytes.
    pub size: usize,
    /// Update-sequence array byte offset.
    pub usa_offset: u16,
    /// Update-sequence number written to protected trailers.
    pub usn: u16,
    /// Raw record flags.
    pub flags: u16,
    /// Record sequence number.
    pub sequence: u16,
    /// Header hard-link count.
    pub hard_links: u16,
    /// Raw base FileRef; zero for a base record.
    pub base: u64,
    /// Stored NTFS 3.1 record number.
    pub record_number: u32,
    /// Log sequence number.
    pub lsn: u64,
    /// Next available attribute instance id.
    pub next_attribute_id: u16,
    /// Encoded, eight-byte-aligned attributes.
    pub attributes: Vec<Vec<u8>>,
}

impl Default for RecordBuilder {
    fn default() -> Self {
        Self {
            size: 1024,
            usa_offset: 0x30,
            usn: 0xa55a,
            flags: 1,
            sequence: 7,
            hard_links: 1,
            base: 0,
            record_number: 42,
            lsn: 123,
            next_attribute_id: 9,
            attributes: Vec::new(),
        }
    }
}

impl RecordBuilder {
    /// Returns the aligned first-attribute offset after the update-sequence array.
    #[must_use]
    pub fn first_attribute(&self) -> usize {
        (usize::from(self.usa_offset) + (self.size / 512 + 1) * 2 + 7) & !7
    }

    /// Encodes the record without applying update-sequence protection.
    ///
    /// # Panics
    ///
    /// Panics if the configured lengths, offsets or values cannot fit the encoded format.
    #[must_use]
    pub fn plain(&self) -> Vec<u8> {
        let mut bytes = vec![0; self.size];
        bytes[..4].copy_from_slice(b"FILE");
        bytes[4..6].copy_from_slice(&self.usa_offset.to_le_bytes());
        bytes[6..8].copy_from_slice(&((self.size / 512 + 1) as u16).to_le_bytes());
        bytes[8..16].copy_from_slice(&self.lsn.to_le_bytes());
        bytes[16..18].copy_from_slice(&self.sequence.to_le_bytes());
        bytes[18..20].copy_from_slice(&self.hard_links.to_le_bytes());
        let first = self.first_attribute();
        bytes[20..22].copy_from_slice(&(first as u16).to_le_bytes());
        bytes[22..24].copy_from_slice(&self.flags.to_le_bytes());
        bytes[28..32].copy_from_slice(&(self.size as u32).to_le_bytes());
        bytes[32..40].copy_from_slice(&self.base.to_le_bytes());
        bytes[40..42].copy_from_slice(&self.next_attribute_id.to_le_bytes());
        if self.usa_offset >= 0x30 {
            bytes[44..48].copy_from_slice(&self.record_number.to_le_bytes());
        }
        let mut cursor = first;
        for attribute in &self.attributes {
            assert_eq!(attribute.len() % 8, 0);
            bytes[cursor..cursor + attribute.len()].copy_from_slice(attribute);
            cursor += attribute.len();
        }
        bytes[cursor..cursor + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        let used = (cursor + 8) as u32;
        bytes[24..28].copy_from_slice(&used.to_le_bytes());
        bytes
    }

    /// Copies a plain record and applies update-sequence protection.
    ///
    /// # Panics
    ///
    /// Panics if the configured lengths, offsets or values cannot fit the encoded format.
    #[must_use]
    pub fn protect(&self, plain: &[u8]) -> Vec<u8> {
        let mut bytes = plain.to_vec();
        let usa = usize::from(self.usa_offset);
        bytes[usa..usa + 2].copy_from_slice(&self.usn.to_le_bytes());
        for stride in 0..self.size / 512 {
            let tail = (stride + 1) * 512 - 2;
            let replacement = usa + (stride + 1) * 2;
            bytes[replacement..replacement + 2].copy_from_slice(&plain[tail..tail + 2]);
            bytes[tail..tail + 2].copy_from_slice(&self.usn.to_le_bytes());
        }
        bytes
    }

    /// Encodes the configured fields into owned bytes.
    ///
    /// # Panics
    ///
    /// Panics if the configured lengths, offsets or values cannot fit the encoded format.
    #[must_use]
    pub fn build(&self) -> Vec<u8> {
        self.protect(&self.plain())
    }

    /// Flips one protected trailer byte for a selected stride.
    ///
    /// # Panics
    ///
    /// Panics if the configured lengths, offsets or values cannot fit the encoded format.
    pub fn corrupt_tail(&self, bytes: &mut [u8], stride: usize) {
        assert!(stride < self.size / 512);
        bytes[(stride + 1) * 512 - 2] ^= 1;
    }
}

#[derive(Debug, Clone)]
/// Raw non-resident descriptor fields, including deliberately invalid test values.
pub struct NonResidentModel {
    /// Lowest VCN of this segment.
    pub lowest: i64,
    /// Highest VCN of this segment.
    pub highest: i64,
    /// Raw compression-unit byte.
    pub compression_unit: u8,
    /// Allocated, data, initialized and compressed sizes, in that order.
    pub sizes: [i64; 4],
    /// Encoded mapping pairs including their terminator.
    pub mapping_pairs: Vec<u8>,
    /// Whether to force the extended size header.
    pub extended_header: bool,
}

impl Default for NonResidentModel {
    fn default() -> Self {
        Self {
            lowest: 0,
            highest: 0,
            compression_unit: 0,
            sizes: [4096, 4096, 4096, 4096],
            mapping_pairs: vec![0x11, 1, 1, 0],
            extended_header: false,
        }
    }
}

#[derive(Debug, Clone)]
/// Resident bytes or a raw non-resident descriptor to encode.
pub enum AttrBody {
    /// Resident value bytes.
    Resident(Vec<u8>),
    /// Non-resident descriptor and mapping pairs.
    NonResident(NonResidentModel),
}

#[derive(Debug, Clone)]
/// Attribute encoder supporting names, padding and both storage forms.
pub struct AttrBuilder {
    /// Raw attribute type code.
    pub type_code: u32,
    /// Attribute instance id.
    pub id: u16,
    /// Raw attribute flags.
    pub flags: u16,
    /// Attribute name as UTF-16 units.
    pub name: Vec<u16>,
    /// Padding before the name.
    pub name_gap: usize,
    /// Padding before the value or mapping pairs.
    pub body_gap: usize,
    /// Storage form and value to encode.
    pub body: AttrBody,
}

impl AttrBuilder {
    /// Creates a resident attribute with the supplied value.
    #[must_use]
    pub fn resident(type_code: u32, value: Vec<u8>) -> Self {
        Self {
            type_code,
            id: 0,
            flags: 0,
            name: Vec::new(),
            name_gap: 0,
            body_gap: 0,
            body: AttrBody::Resident(value),
        }
    }

    /// Creates a non-resident attribute from raw descriptor fields.
    #[must_use]
    pub fn non_resident(type_code: u32, model: NonResidentModel) -> Self {
        Self {
            body: AttrBody::NonResident(model),
            ..Self::resident(type_code, Vec::new())
        }
    }

    /// Encodes the configured fields into owned bytes.
    ///
    /// # Panics
    ///
    /// Panics if the configured lengths, offsets or values cannot fit the encoded format.
    #[must_use]
    pub fn build(&self) -> Vec<u8> {
        assert!(self.name.len() <= 255);
        let header = match &self.body {
            AttrBody::Resident(_) => 24,
            AttrBody::NonResident(model) if model.extended_header || self.flags & 0x80ff != 0 => 72,
            AttrBody::NonResident(_) => 64,
        };
        let name_offset = header + self.name_gap;
        let body_offset = name_offset + self.name.len() * 2 + self.body_gap;
        let body = match &self.body {
            AttrBody::Resident(value) => value,
            AttrBody::NonResident(model) => &model.mapping_pairs,
        };
        let length = (body_offset + body.len() + 7) & !7;
        let mut bytes = vec![0; length];
        bytes[..4].copy_from_slice(&self.type_code.to_le_bytes());
        bytes[4..8].copy_from_slice(&(length as u32).to_le_bytes());
        bytes[8] = u8::from(matches!(self.body, AttrBody::NonResident(_)));
        bytes[9] = self.name.len() as u8;
        bytes[10..12].copy_from_slice(&(name_offset as u16).to_le_bytes());
        bytes[12..14].copy_from_slice(&self.flags.to_le_bytes());
        bytes[14..16].copy_from_slice(&self.id.to_le_bytes());
        for (index, unit) in self.name.iter().enumerate() {
            bytes[name_offset + index * 2..name_offset + index * 2 + 2]
                .copy_from_slice(&unit.to_le_bytes());
        }
        match &self.body {
            AttrBody::Resident(value) => {
                bytes[16..20].copy_from_slice(&(value.len() as u32).to_le_bytes());
                bytes[20..22].copy_from_slice(&(body_offset as u16).to_le_bytes());
            }
            AttrBody::NonResident(model) => {
                bytes[16..24].copy_from_slice(&model.lowest.to_le_bytes());
                bytes[24..32].copy_from_slice(&model.highest.to_le_bytes());
                bytes[32..34].copy_from_slice(&(body_offset as u16).to_le_bytes());
                bytes[34] = model.compression_unit;
                for (index, value) in model
                    .sizes
                    .iter()
                    .take(if header == 72 { 4 } else { 3 })
                    .enumerate()
                {
                    bytes[40 + index * 8..48 + index * 8].copy_from_slice(&value.to_le_bytes());
                }
            }
        }
        bytes[body_offset..body_offset + body.len()].copy_from_slice(body);
        bytes
    }
}

#[derive(Debug, Clone)]
/// FILE_NAME value encoder retaining cached sizes and raw name units.
pub struct FileNameBuilder {
    /// Raw parent FileRef.
    pub parent: u64,
    /// Created, modified, changed and accessed FILETIMEs.
    pub times: [u64; 4],
    /// Cached allocated size.
    pub allocated: u64,
    /// Cached logical data size.
    pub data: u64,
    /// Raw file attributes.
    pub attributes: u32,
    /// EA-size or reparse-tag word.
    pub ea_or_reparse: u32,
    /// Raw namespace byte.
    pub namespace: u8,
    /// Filename as UTF-16 code units.
    pub name: Vec<u16>,
}
impl Default for FileNameBuilder {
    fn default() -> Self {
        Self {
            parent: (5u64 << 48) | 5,
            times: [1, 2, 3, 4],
            allocated: 4096,
            data: 123,
            attributes: 0x20,
            ea_or_reparse: 0,
            namespace: 1,
            name: "notes.txt".encode_utf16().collect(),
        }
    }
}
impl FileNameBuilder {
    /// Encodes the configured fields into owned bytes.
    ///
    /// # Panics
    ///
    /// Panics if the configured lengths, offsets or values cannot fit the encoded format.
    #[must_use]
    pub fn build(&self) -> Vec<u8> {
        let mut bytes = vec![0; 66 + 2 * self.name.len()];
        bytes[..8].copy_from_slice(&self.parent.to_le_bytes());
        for (index, time) in self.times.iter().enumerate() {
            bytes[8 + index * 8..16 + index * 8].copy_from_slice(&time.to_le_bytes());
        }
        bytes[40..48].copy_from_slice(&self.allocated.to_le_bytes());
        bytes[48..56].copy_from_slice(&self.data.to_le_bytes());
        bytes[56..60].copy_from_slice(&self.attributes.to_le_bytes());
        bytes[60..64].copy_from_slice(&self.ea_or_reparse.to_le_bytes());
        bytes[64] = self.name.len() as u8;
        bytes[65] = self.namespace;
        for (index, unit) in self.name.iter().enumerate() {
            bytes[66 + index * 2..68 + index * 2].copy_from_slice(&unit.to_le_bytes());
        }
        bytes
    }
}

#[derive(Debug, Clone)]
/// Raw ATTRIBUTE_LIST segment reference for independent encoding.
pub struct AttrListEntryModel {
    /// Referenced attribute type.
    pub type_code: u32,
    /// Lowest VCN of the referenced segment.
    pub lowest: i64,
    /// Raw segment FileRef.
    pub segment: u64,
    /// Referenced attribute instance id.
    pub id: u16,
    /// Referenced attribute name as UTF-16 units.
    pub name: Vec<u16>,
}
impl Default for AttrListEntryModel {
    fn default() -> Self {
        Self {
            type_code: 0x80,
            lowest: 1000,
            segment: (1u64 << 48) | 17,
            id: 9,
            name: Vec::new(),
        }
    }
}
#[derive(Debug, Clone, Default)]
/// ATTRIBUTE_LIST encoder with optional zero padding.
pub struct AttrListBuilder {
    /// Entries to encode in the supplied order.
    pub entries: Vec<AttrListEntryModel>,
    /// Zero bytes appended after all entries.
    pub zero_tail: usize,
}
impl AttrListBuilder {
    /// Encodes the configured fields into owned bytes.
    ///
    /// # Panics
    ///
    /// Panics if the configured lengths, offsets or values cannot fit the encoded format.
    #[must_use]
    pub fn build(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        for entry in &self.entries {
            assert!(entry.name.len() <= 255);
            let length = (26 + entry.name.len() * 2 + 7) & !7;
            let mut value = vec![0; length];
            value[..4].copy_from_slice(&entry.type_code.to_le_bytes());
            value[4..6].copy_from_slice(&(length as u16).to_le_bytes());
            value[6] = entry.name.len() as u8;
            value[7] = 26;
            value[8..16].copy_from_slice(&entry.lowest.to_le_bytes());
            value[16..24].copy_from_slice(&entry.segment.to_le_bytes());
            value[24..26].copy_from_slice(&entry.id.to_le_bytes());
            for (index, unit) in entry.name.iter().enumerate() {
                value[26 + index * 2..28 + index * 2].copy_from_slice(&unit.to_le_bytes());
            }
            bytes.extend(value);
        }
        bytes.resize(bytes.len() + self.zero_tail, 0);
        bytes
    }
}
