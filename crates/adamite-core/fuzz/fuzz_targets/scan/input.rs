//! Bounded input decoding and conservative guards before calling the scan driver.

use adamite_core::ScanConfig;
use adamite_ntfs::{AttributeType, RecordClass, classify, parse_boot_sector};
use libfuzzer_sys::arbitrary::{self, Unstructured};

pub(super) const MAX_IMAGE: usize = 1_048_576;
const MAX_RECORDS: u64 = 4096;
const MAX_LIST: u64 = 262_144;
const SECTOR: usize = 512;

#[derive(Debug)]
pub(super) struct Input<'a> {
    mode: u8,
    transport: u8,
    layout: u8,
    pub(super) events: [u8; 8],
    mutations: Vec<(u32, u8)>,
    raw: &'a [u8],
}

impl<'a> Input<'a> {
    pub(super) fn parse(bytes: &'a [u8]) -> arbitrary::Result<Self> {
        let mut source = Unstructured::new(bytes);
        let mode = source.arbitrary()?;
        let transport = source.arbitrary()?;
        let layout = source.arbitrary()?;
        let events = source.arbitrary()?;
        let count = source.arbitrary::<u8>()? % 17;
        let mut mutations = Vec::with_capacity(usize::from(count));
        for _ in 0..count {
            mutations.push((source.arbitrary()?, source.arbitrary()?));
        }
        Ok(Self {
            mode,
            transport,
            layout,
            events,
            mutations,
            raw: source.take_rest(),
        })
    }

    pub(super) fn image(&self) -> Vec<u8> {
        let mut image = match self.mode % 3 {
            0 => super::volume::build(super::volume::Layout::Contiguous, self.layout),
            1 => super::volume::build(super::volume::Layout::Fragmented, self.layout),
            _ => self.raw[..self.raw.len().min(MAX_IMAGE)].to_vec(),
        };
        if !image.is_empty() {
            for &(offset, xor) in &self.mutations {
                // Offsets are interpreted after the image has been bounded.
                let offset =
                    usize::try_from(offset).expect("u32 fits supported hosts") % image.len();
                image[offset] ^= xor;
            }
        }
        image
    }

    pub(super) fn config(&self) -> ScanConfig {
        ScanConfig {
            max_outstanding: usize::from(self.transport & 3) + 1,
            chunk_size: 512usize << ((self.transport >> 2) & 3),
            ..ScanConfig::default()
        }
    }
}

fn u64_at(bytes: &[u8], offset: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        bytes.get(offset..offset.checked_add(8)?)?.try_into().ok()?,
    ))
}

pub(super) fn preflight(image: &[u8]) -> Option<usize> {
    assert!(image.len() <= MAX_IMAGE);
    // Attribute headers are eight-byte aligned. Reject possible list headers whose
    // fixed fields cross a sector: their bytes may have fixups or a different extent.
    // This also covers extension records without discovering their MFT mapping first.
    for start in (0..image.len()).step_by(8) {
        if image.get(start..start + 4) != Some(&[0x20, 0, 0, 0]) {
            continue;
        }
        let Some(length) = image
            .get(start + 4..start + 8)
            .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
            .map(u32::from_le_bytes)
        else {
            continue;
        };
        // Ignore payload words that cannot be attribute lengths. At offset 504,
        // fixup protection replaces the length's high half, so only its low half is known.
        let plausible = if start % SECTOR == SECTOR - 8 {
            let low = length & 0xffff;
            low.is_multiple_of(8) && (low == 0 || low >= 24)
        } else {
            (24..=65_536).contains(&length) && length.is_multiple_of(8)
        };
        if !plausible {
            continue;
        }
        if start % SECTOR > SECTOR - 64 {
            return None;
        }
        match image.get(start + 8) {
            Some(0) => {
                if image
                    .get(start + 16..start + 20)
                    .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
                    .is_some_and(|bytes| u64::from(u32::from_le_bytes(bytes)) > MAX_LIST)
                {
                    return None;
                }
            }
            Some(1) if u64_at(image, start + 48).is_some_and(|size| size > MAX_LIST) => {
                return None;
            }
            _ => {}
        }
    }
    let Ok(geometry) = parse_boot_sector(image) else {
        return Some(0);
    };
    let start = geometry
        .mft_lcn()
        .checked_mul(u64::from(geometry.cluster_size()))?;
    let start = usize::try_from(start).ok()?;
    let record_size = usize::try_from(geometry.file_record_size()).ok()?;
    let Some(raw) = image.get(start..start.checked_add(record_size)?) else {
        // The driver stops before feeding a record outside the supplied image.
        return Some(0);
    };
    let mut bytes = raw.to_vec();
    let RecordClass::Base(record) = classify(&mut bytes) else {
        return Some(0);
    };
    let mut records = 0;
    for attribute in record.attributes().flatten() {
        if attribute.attribute_type() == AttributeType::DATA
            && attribute.name().len_units() == 0
            && let Some(segment) = attribute.non_resident()
            && let Some(sizes) = segment.sizes()
        {
            let count = sizes
                .data()
                .div_ceil(u64::from(geometry.file_record_size()));
            if count > MAX_RECORDS {
                return None;
            }
            records = records.max(usize::try_from(count).ok()?);
        }
    }
    Some(records)
}
