//! Record-zero validation and owned inputs for subsequent extension discovery.

use super::{
    mapping::{MftMapping, OwnedRun, overflow},
    scanner::ScanError,
};
use adamite_ntfs::{
    AttributeFlags, AttributeType, FileRecord, FileRef, NonResident, RecordFlags, StreamSizes,
    VolumeGeometry, fixup,
};

/// Raw descriptor bytes are owned; ATTRIBUTE_LIST values are interpreted in M6.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum ListStorage {
    Resident(Vec<u8>),
    NonResident {
        lowest_vcn: u64,
        highest_vcn: i64,
        compression_unit: u8,
        sizes: Option<StreamSizes>,
        mapping_pairs: Vec<u8>,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct AttributeListDescriptor {
    pub id: u16,
    pub name: Vec<u16>,
    pub flags: AttributeFlags,
    pub storage: ListStorage,
}

#[derive(Debug)]
pub(super) struct Record0 {
    pub reference: FileRef,
    pub data_id: u16,
    pub data_size: u64,
    pub mapping: MftMapping,
    pub attribute_lists: Vec<AttributeListDescriptor>,
}

fn copy_slice<T: Copy>(input: &[T]) -> Result<Vec<T>, ScanError> {
    let mut owned = Vec::new();
    owned
        .try_reserve_exact(input.len())
        .map_err(|source| ScanError::Allocation { source })?;
    owned.extend_from_slice(input);
    Ok(owned)
}

fn collect_runs(
    segment: NonResident<'_>,
    geometry: VolumeGeometry,
) -> Result<MftMapping, ScanError> {
    let mut runs = Vec::new();
    // The final iterator step verifies termination and the declared VCN coverage.
    for run in segment.runs() {
        let run = run.map_err(|source| ScanError::Runlist { source })?;
        let lcn = run.lcn().ok_or(ScanError::Bootstrap {
            reason: "sparse MFT run",
        })?;
        runs.try_reserve(1)
            .map_err(|source| ScanError::Allocation { source })?;
        runs.push(OwnedRun {
            vcn: run.vcn(),
            lcn,
            clusters: run.length(),
        });
    }
    MftMapping::new(geometry, runs)
}

pub(super) fn parse_record0(
    mut bytes: Vec<u8>,
    geometry: VolumeGeometry,
) -> Result<Record0, ScanError> {
    let fixed = fixup::apply(&mut bytes).map_err(|source| ScanError::Fixup { source })?;
    let record = FileRecord::parse(fixed).map_err(|source| ScanError::Record { source })?;
    let header = record.header();
    if !header.flags().contains(RecordFlags::IN_USE)
        || !header.base_reference().is_null()
        || header.sequence() == 0
        || header.record_number().is_some_and(|ordinal| ordinal != 0)
    {
        return Err(ScanError::Bootstrap {
            reason: "record zero must be an in-use base with its own identity and nonzero sequence",
        });
    }
    let mut first = None;
    let mut attribute_lists = Vec::new();
    for attribute in record.attributes() {
        let attribute = attribute.map_err(|source| ScanError::Attribute { source })?;
        if attribute.attribute_type() == AttributeType::DATA && attribute.name().len_units() == 0 {
            let segment = attribute.non_resident().ok_or(ScanError::Bootstrap {
                reason: "resident MFT DATA",
            })?;
            if attribute.flags().compression_format() != 0
                || attribute.flags().contains(AttributeFlags::SPARSE)
                || attribute.flags().contains(AttributeFlags::ENCRYPTED)
                || segment.compression_unit() != 0
            {
                return Err(ScanError::Bootstrap {
                    reason: "unsupported MFT DATA storage",
                });
            }
            let mapping = collect_runs(segment, geometry)?;
            if segment.lowest_vcn() == 0 {
                if first.is_some() {
                    return Err(ScanError::Bootstrap {
                        reason: "duplicate first MFT DATA segment",
                    });
                }
                let sizes = segment.sizes().ok_or(ScanError::Bootstrap {
                    reason: "missing MFT stream sizes",
                })?;
                if sizes.data() == 0
                    || !sizes
                        .data()
                        .is_multiple_of(u64::from(geometry.file_record_size()))
                {
                    return Err(ScanError::Bootstrap {
                        reason: "MFT data size must be positive and record aligned",
                    });
                }
                first = Some((attribute.id(), sizes.data(), mapping));
            }
        } else if attribute.attribute_type() == AttributeType::ATTRIBUTE_LIST {
            let storage = match attribute.resident_value() {
                Some(value) => ListStorage::Resident(copy_slice(value)?),
                None => {
                    let segment = attribute.non_resident().ok_or(ScanError::Bootstrap {
                        reason: "missing attribute-list descriptor",
                    })?;
                    ListStorage::NonResident {
                        lowest_vcn: segment.lowest_vcn(),
                        highest_vcn: segment.highest_vcn(),
                        compression_unit: segment.compression_unit(),
                        sizes: segment.sizes(),
                        mapping_pairs: copy_slice(segment.mapping_pairs())?,
                    }
                }
            };
            let mut name = Vec::new();
            name.try_reserve_exact(attribute.name().len_units())
                .map_err(|source| ScanError::Allocation { source })?;
            name.extend(attribute.name().units());
            attribute_lists
                .try_reserve(1)
                .map_err(|source| ScanError::Allocation { source })?;
            attribute_lists.push(AttributeListDescriptor {
                id: attribute.id(),
                name,
                flags: attribute.flags(),
                storage,
            });
        }
    }
    let (data_id, data_size, mapping) = first.ok_or(ScanError::Bootstrap {
        reason: "missing unnamed MFT DATA first segment",
    })?;
    let boot_start = geometry
        .mft_lcn()
        .checked_mul(u64::from(geometry.cluster_size()))
        .ok_or_else(|| overflow("record-zero offset"))?;
    if mapping.virtual_offset(boot_start)? != 0 {
        return Err(ScanError::Bootstrap {
            reason: "MFT first run disagrees with boot location",
        });
    }
    // Confirm that the bytes already read are exactly the record's self-described range.
    let mut cursor = 0u64;
    let record_len = u64::from(geometry.file_record_size());
    while cursor < record_len {
        let remaining = usize::try_from(
            record_len
                .checked_sub(cursor)
                .ok_or_else(|| overflow("record-zero remainder"))?,
        )
        .map_err(|_| overflow("record-zero length"))?;
        let piece = mapping.read_piece(cursor, remaining)?;
        if boot_start.checked_add(cursor) != Some(piece.offset) {
            return Err(ScanError::Bootstrap {
                reason: "record zero is not contiguous at the boot location",
            });
        }
        cursor = cursor
            .checked_add(u64::try_from(piece.len).map_err(|_| overflow("record-zero piece"))?)
            .ok_or_else(|| overflow("record-zero cursor"))?;
    }
    Ok(Record0 {
        reference: FileRef::from_raw(u64::from(header.sequence()) << 48),
        data_id,
        data_size,
        mapping,
        attribute_lists,
    })
}
