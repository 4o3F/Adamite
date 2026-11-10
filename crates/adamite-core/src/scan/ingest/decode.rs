//! Transactional decoding and parser diagnostic categorization.

use super::{bump, stream_error};
use crate::{
    ScanStats,
    entry::{
        NameAttribute, RecordContribution, ReparseAttribute, StandardAttribute, StreamSegment,
        StreamStorage,
    },
    scan::{bootstrap::push, scanner::ScanError},
};
use adamite_ntfs::{
    AttributeList, AttributeType, FileName, FileRecord, FileRef, IndexAllocation, ReparsePoint,
    StandardInformation, Utf16Le, ValueError, VolumeGeometry,
};

pub(super) enum DecodeError {
    Fatal(ScanError),
    Attributes,
    Value(ValueError),
    Runlist,
    Extent,
    List,
}

impl From<ScanError> for DecodeError {
    fn from(error: ScanError) -> Self {
        Self::Fatal(error)
    }
}

pub(super) fn value_counter(error: ValueError, stats: &mut ScanStats) -> Result<(), ScanError> {
    match error {
        ValueError::WrongAttributeType { .. } | ValueError::WrongStorageForm => {
            bump(&mut stats.type_storage_mismatches)
        }
        _ => bump(&mut stats.corrupt_values),
    }
}

fn name(value: Utf16Le<'_>) -> Result<Vec<u16>, ScanError> {
    let mut units = Vec::new();
    units
        .try_reserve_exact(value.len_units())
        .map_err(|source| ScanError::Allocation { source })?;
    units.extend(value.units());
    Ok(units)
}

pub(super) fn contribution(
    record: &FileRecord<'_>,
    reference: FileRef,
    geometry: VolumeGeometry,
) -> Result<RecordContribution, DecodeError> {
    let mut owned = RecordContribution {
        reference,
        hard_links: record.header().hard_links(),
        names: Vec::new(),
        standard_information: Vec::new(),
        streams: Vec::new(),
        reparse_tags: Vec::new(),
    };
    for attribute in record.attributes() {
        let attribute = attribute.map_err(|_| DecodeError::Attributes)?;
        match attribute.attribute_type() {
            AttributeType::FILE_NAME => {
                let value = FileName::from_attribute(attribute).map_err(DecodeError::Value)?;
                push(
                    &mut owned.names,
                    NameAttribute {
                        attribute_id: attribute.id(),
                        parent: value.parent(),
                        namespace: value.namespace(),
                        name: name(value.name())?,
                    },
                )?;
            }
            AttributeType::STANDARD_INFORMATION => {
                let value =
                    StandardInformation::from_attribute(attribute).map_err(DecodeError::Value)?;
                push(
                    &mut owned.standard_information,
                    StandardAttribute {
                        attribute_id: attribute.id(),
                        value,
                    },
                )?;
            }
            AttributeType::REPARSE_POINT => {
                let value = ReparsePoint::from_attribute(attribute).map_err(DecodeError::Value)?;
                push(
                    &mut owned.reparse_tags,
                    ReparseAttribute {
                        attribute_id: attribute.id(),
                        tag: value.tag(),
                    },
                )?;
            }
            AttributeType::INDEX_ALLOCATION => {
                IndexAllocation::from_attribute(attribute).map_err(DecodeError::Value)?;
            }
            AttributeType::ATTRIBUTE_LIST => {
                if let Some(value) = attribute.resident_value() {
                    for entry in AttributeList::new(value).map_err(|_| DecodeError::List)? {
                        entry.map_err(|_| DecodeError::List)?;
                    }
                }
            }
            _ => {}
        }
        if attribute.attribute_type() == AttributeType::DATA || attribute.non_resident().is_some() {
            let storage = if let Some(segment) = attribute.non_resident() {
                let mut run_count = 0u64;
                for run in segment.runs() {
                    let run = run.map_err(|_| DecodeError::Runlist)?;
                    run.validate_geometry(&geometry)
                        .map_err(|_| DecodeError::Extent)?;
                    bump(&mut run_count)?;
                }
                StreamStorage::NonResident {
                    lowest_vcn: segment.lowest_vcn(),
                    highest_vcn: segment.highest_vcn(),
                    compression_unit: segment.compression_unit(),
                    sizes: segment.sizes(),
                    run_count,
                }
            } else {
                let value = attribute
                    .resident_value()
                    .ok_or_else(|| stream_error("missing resident stream value"))?;
                StreamStorage::Resident {
                    size: u64::try_from(value.len())
                        .map_err(|_| stream_error("resident stream size is unrepresentable"))?,
                }
            };
            push(
                &mut owned.streams,
                StreamSegment {
                    source: reference,
                    attribute_type: attribute.attribute_type(),
                    name: name(attribute.name())?,
                    attribute_id: attribute.id(),
                    flags: attribute.flags(),
                    storage,
                },
            )?;
        }
    }
    Ok(owned)
}
