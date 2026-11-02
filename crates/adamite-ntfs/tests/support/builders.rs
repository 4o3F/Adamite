#![allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

//! Independent on-disk encoders used by parser integration tests.

#[derive(Debug, Clone)]
pub struct BootSectorBuilder {
    pub sector: u16,
    pub cluster_encoding: u8,
    pub file_encoding: i8,
    pub index_encoding: i8,
    pub sectors: u64,
    pub mft: u64,
    pub mirror: u64,
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
pub struct RecordBuilder {
    pub size: usize,
    pub usa_offset: u16,
    pub usn: u16,
    pub flags: u16,
    pub sequence: u16,
    pub hard_links: u16,
    pub base: u64,
    pub record_number: u32,
    pub lsn: u64,
    pub next_attribute_id: u16,
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
    pub fn first_attribute(&self) -> usize {
        (usize::from(self.usa_offset) + (self.size / 512 + 1) * 2 + 7) & !7
    }

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

    pub fn build(&self) -> Vec<u8> {
        self.protect(&self.plain())
    }

    pub fn corrupt_tail(&self, bytes: &mut [u8], stride: usize) {
        assert!(stride < self.size / 512);
        bytes[(stride + 1) * 512 - 2] ^= 1;
    }
}

#[derive(Debug, Clone)]
pub struct NonResidentModel {
    pub lowest: i64,
    pub highest: i64,
    pub compression_unit: u8,
    pub sizes: [i64; 4],
    pub mapping_pairs: Vec<u8>,
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
pub enum AttrBody {
    Resident(Vec<u8>),
    NonResident(NonResidentModel),
}

#[derive(Debug, Clone)]
pub struct AttrBuilder {
    pub type_code: u32,
    pub id: u16,
    pub flags: u16,
    pub name: Vec<u16>,
    pub name_gap: usize,
    pub body_gap: usize,
    pub body: AttrBody,
}

impl AttrBuilder {
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

    pub fn non_resident(type_code: u32, model: NonResidentModel) -> Self {
        Self {
            body: AttrBody::NonResident(model),
            ..Self::resident(type_code, Vec::new())
        }
    }

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
pub struct FileNameBuilder {
    pub parent: u64,
    pub times: [u64; 4],
    pub allocated: u64,
    pub data: u64,
    pub attributes: u32,
    pub ea_or_reparse: u32,
    pub namespace: u8,
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
pub struct AttrListEntryModel {
    pub type_code: u32,
    pub lowest: i64,
    pub segment: u64,
    pub id: u16,
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
pub struct AttrListBuilder {
    pub entries: Vec<AttrListEntryModel>,
    pub zero_tail: usize,
}
impl AttrListBuilder {
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
