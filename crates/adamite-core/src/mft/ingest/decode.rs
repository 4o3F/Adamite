//! Transactional decoding and parser diagnostic categorization.

use crate::{
    error::ScanError,
    mft::model::{
        NameAttribute, RecordContribution, ReparseAttribute, StandardAttribute, StreamSegment,
        StreamStorage,
    },
    scan::ScanStats,
    util::{CounterExt, VecExt},
};
use adamite_ntfs::{
    attribute::AttributeType,
    boot::VolumeGeometry,
    record::{FileRecord, reference::FileRef},
    utf16::Utf16Le,
    value::{
        ValueError, attribute_list::AttributeList, file_name::FileName,
        index_allocation::IndexAllocation, reparse_point::ReparsePoint,
        standard_information::StandardInformation,
    },
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

impl DecodeError {
    /// Counts a discarded contribution, propagating allocation and arithmetic failures.
    pub(super) fn account(self, stats: &mut ScanStats) -> Result<(), ScanError> {
        match self {
            DecodeError::Fatal(error) => return Err(error),
            DecodeError::Attributes => stats.corrupt_attributes.try_bump()?,
            DecodeError::Value(error) => {
                let counter = match error {
                    ValueError::WrongAttributeType { .. } | ValueError::WrongStorageForm => {
                        &mut stats.type_storage_mismatches
                    }
                    _ => &mut stats.corrupt_values,
                };
                counter.try_bump()?;
            }
            DecodeError::Runlist => stats.corrupt_runlists.try_bump()?,
            DecodeError::Extent => stats.out_of_volume_extents.try_bump()?,
            DecodeError::List => stats.invalid_attribute_lists.try_bump()?,
        }
        stats.corrupt_records.try_bump()?;
        Ok(())
    }
}

fn name(value: Utf16Le<'_>) -> Result<Vec<u16>, ScanError> {
    let mut units = Vec::new();
    units.try_reserve_exact(value.len_units())?;
    units.extend(value.units());
    Ok(units)
}

impl RecordContribution {
    pub(super) fn decode(
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
                    owned
                        .names
                        .try_push(NameAttribute {
                            parent: value.parent(),
                            namespace: value.namespace(),
                            name: name(value.name())?,
                        })
                        .map_err(ScanError::from)?;
                }
                AttributeType::STANDARD_INFORMATION => {
                    let value = StandardInformation::from_attribute(attribute)
                        .map_err(DecodeError::Value)?;
                    owned
                        .standard_information
                        .try_push(StandardAttribute { value })
                        .map_err(ScanError::from)?;
                }
                AttributeType::REPARSE_POINT => {
                    let value =
                        ReparsePoint::from_attribute(attribute).map_err(DecodeError::Value)?;
                    owned
                        .reparse_tags
                        .try_push(ReparseAttribute { tag: value.tag() })
                        .map_err(ScanError::from)?;
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
            if attribute.attribute_type() == AttributeType::DATA
                || attribute.non_resident().is_some()
            {
                let storage = if let Some(segment) = attribute.non_resident() {
                    let mut run_count = 0u64;
                    let mut mapped_clusters = 0u64;
                    for run in segment.runs() {
                        let run = run.map_err(|_| DecodeError::Runlist)?;
                        run.validate_geometry(&geometry)
                            .map_err(|_| DecodeError::Extent)?;
                        if run.lcn().is_some() {
                            mapped_clusters = mapped_clusters
                                .checked_add(run.length())
                                .ok_or_else(|| ScanError::overflow("stream mapped clusters"))?;
                        }
                        run_count.try_bump().map_err(ScanError::from)?;
                    }
                    StreamStorage::NonResident {
                        lowest_vcn: segment.lowest_vcn(),
                        highest_vcn: segment.highest_vcn(),
                        compression_unit: segment.compression_unit(),
                        sizes: segment.sizes(),
                        run_count,
                        mapped_clusters,
                    }
                } else {
                    let value = attribute
                        .resident_value()
                        .ok_or_else(|| ScanError::stream("missing resident stream value"))?;
                    StreamStorage::Resident {
                        size: u64::try_from(value.len()).map_err(|_| {
                            ScanError::stream("resident stream size is unrepresentable")
                        })?,
                    }
                };
                owned
                    .streams
                    .try_push(StreamSegment {
                        source: reference,
                        attribute_type: attribute.attribute_type(),
                        name: name(attribute.name())?,
                        attribute_id: attribute.id(),
                        flags: attribute.flags(),
                        storage,
                    })
                    .map_err(ScanError::from)?;
            }
        }
        Ok(owned)
    }
}
