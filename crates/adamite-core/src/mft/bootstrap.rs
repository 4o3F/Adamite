//! Validated record-zero and extension contributions for MFT discovery.

use super::mapping::{MftMapping, OwnedRun};
use crate::{
    error::ScanError,
    util::{SliceExt, VecExt},
};
use adamite_ntfs::{
    attribute::{
        Attribute, AttributeFlags, AttributeType,
        non_resident::{NonResident, StreamSizes},
    },
    boot::VolumeGeometry,
    record::{FileRecord, RecordFlags, reference::FileRef},
};

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ListStorage {
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
pub(crate) struct AttributeListDescriptor {
    pub id: u16,
    pub name: Vec<u16>,
    pub flags: AttributeFlags,
    pub storage: ListStorage,
}

#[derive(Debug)]
pub(crate) struct DataSegment {
    pub id: u16,
    pub lowest_vcn: u64,
    pub sizes: Option<StreamSizes>,
    pub runs: Vec<OwnedRun>,
}

#[derive(Debug)]
pub(crate) struct RecordParts {
    pub reference: FileRef,
    pub segments: Vec<DataSegment>,
    pub lists: Vec<AttributeListDescriptor>,
}

#[derive(Debug)]
pub(crate) struct Record0 {
    pub reference: FileRef,
    pub data_id: u16,
    pub data_size: u64,
    pub mapping: MftMapping,
    pub attribute_lists: Vec<AttributeListDescriptor>,
    pub segments: Vec<DataSegment>,
}

impl MftMapping {
    fn check_storage(flags: AttributeFlags, compression_unit: u8) -> Result<(), ScanError> {
        if flags.compression_format() != 0
            || flags.contains(AttributeFlags::SPARSE)
            || flags.contains(AttributeFlags::ENCRYPTED)
            || compression_unit != 0
        {
            return Err(ScanError::bootstrap(
                "sparse, compressed or encrypted bootstrap storage",
            ));
        }
        Ok(())
    }

    pub(super) fn from_segment(
        segment: NonResident<'_>,
        flags: AttributeFlags,
        geometry: VolumeGeometry,
    ) -> Result<MftMapping, ScanError> {
        Self::check_storage(flags, segment.compression_unit())?;
        Self::from_runlist(segment.runs(), geometry)
    }

    pub(super) fn from_runlist(
        runs: adamite_ntfs::attribute::runlist::Runlist<'_>,
        geometry: VolumeGeometry,
    ) -> Result<MftMapping, ScanError> {
        let mut owned = Vec::new();
        // Exhaust the iterator so its terminator and VCN coverage are validated too.
        for run in runs {
            let run = run.map_err(|source| ScanError::Runlist { source })?;
            let lcn = run
                .lcn()
                .ok_or_else(|| ScanError::bootstrap("sparse bootstrap run"))?;
            owned.try_push(OwnedRun {
                vcn: run.vcn(),
                lcn,
                clusters: run.length(),
            })?;
        }
        MftMapping::new(geometry, owned)
    }
}

impl AttributeListDescriptor {
    fn from_attribute(attribute: Attribute<'_>) -> Result<AttributeListDescriptor, ScanError> {
        let storage = match attribute.resident_value() {
            Some(value) => ListStorage::Resident(value.try_copy()?),
            None => {
                let segment = attribute
                    .non_resident()
                    .ok_or_else(|| ScanError::bootstrap("missing attribute-list descriptor"))?;
                ListStorage::NonResident {
                    lowest_vcn: segment.lowest_vcn(),
                    highest_vcn: segment.highest_vcn(),
                    compression_unit: segment.compression_unit(),
                    sizes: segment.sizes(),
                    mapping_pairs: segment.mapping_pairs().try_copy()?,
                }
            }
        };
        let mut name = Vec::new();
        name.try_reserve_exact(attribute.name().len_units())?;
        name.extend(attribute.name().units());
        Ok(AttributeListDescriptor {
            id: attribute.id(),
            name,
            flags: attribute.flags(),
            storage,
        })
    }

    pub(super) fn lowest_vcn(&self) -> u64 {
        match self.storage {
            ListStorage::Resident(_) => 0,
            ListStorage::NonResident { lowest_vcn, .. } => lowest_vcn,
        }
    }

    pub(super) fn validate_storage(&self) -> Result<(), ScanError> {
        let compression_unit = match self.storage {
            ListStorage::Resident(_) => 0,
            ListStorage::NonResident {
                compression_unit, ..
            } => compression_unit,
        };
        MftMapping::check_storage(self.flags, compression_unit)
    }
}

impl RecordParts {
    fn from_record(
        record: &FileRecord<'_>,
        reference: FileRef,
        geometry: VolumeGeometry,
    ) -> Result<RecordParts, ScanError> {
        let mut segments = Vec::new();
        let mut lists = Vec::new();
        for attribute in record.attributes() {
            let attribute = attribute.map_err(|source| ScanError::Attribute { source })?;
            if attribute.attribute_type() == AttributeType::DATA
                && attribute.name().len_units() == 0
            {
                let segment = attribute
                    .non_resident()
                    .ok_or_else(|| ScanError::bootstrap("resident MFT DATA"))?;
                let mapping = MftMapping::from_segment(segment, attribute.flags(), geometry)?;
                segments.try_push(DataSegment {
                    id: attribute.id(),
                    lowest_vcn: segment.lowest_vcn(),
                    sizes: segment.sizes(),
                    runs: mapping.runs,
                })?;
            } else if attribute.attribute_type() == AttributeType::ATTRIBUTE_LIST {
                lists.try_push(AttributeListDescriptor::from_attribute(attribute)?)?;
            }
        }
        Ok(RecordParts {
            reference,
            segments,
            lists,
        })
    }

    pub(crate) fn parse_extension(
        mut bytes: Vec<u8>,
        reference: FileRef,
        base: FileRef,
        geometry: VolumeGeometry,
    ) -> Result<RecordParts, ScanError> {
        let record = FileRecord::parse(&mut bytes).map_err(ScanError::bootstrap_record)?;
        let header = record.header();
        if !header.flags().contains(RecordFlags::IN_USE)
            || header.sequence() != reference.sequence()
            || header.base_reference() != base
            || header
                .record_number()
                .is_some_and(|ordinal| u64::from(ordinal) != reference.record())
        {
            return Err(ScanError::bootstrap(
                "stale or invalid MFT extension identity or base reference",
            ));
        }
        Self::from_record(&record, reference, geometry)
    }
}

impl Record0 {
    pub(crate) fn parse(
        mut bytes: Vec<u8>,
        geometry: VolumeGeometry,
    ) -> Result<Record0, ScanError> {
        let record = FileRecord::parse(&mut bytes).map_err(ScanError::bootstrap_record)?;
        let header = record.header();
        if !header.flags().contains(RecordFlags::IN_USE)
            || !header.base_reference().is_null()
            || header.sequence() == 0
            || header.record_number().is_some_and(|ordinal| ordinal != 0)
        {
            return Err(ScanError::bootstrap(
                "record zero must be an in-use base with its own identity and nonzero sequence",
            ));
        }
        let reference = FileRef::from_raw(u64::from(header.sequence()) << 48);
        let parts = RecordParts::from_record(&record, reference, geometry)?;
        let mut first = parts
            .segments
            .iter()
            .filter(|segment| segment.lowest_vcn == 0);
        let segment = first
            .next()
            .ok_or_else(|| ScanError::bootstrap("missing unnamed MFT DATA first segment"))?;
        if first.next().is_some() {
            return Err(ScanError::bootstrap("duplicate first MFT DATA segment"));
        }
        let data_size = segment
            .sizes
            .ok_or_else(|| ScanError::bootstrap("missing MFT stream sizes"))?
            .data();
        if data_size == 0 || !data_size.is_multiple_of(u64::from(geometry.file_record_size())) {
            return Err(ScanError::bootstrap(
                "MFT data size must be positive and record aligned",
            ));
        }
        let data_id = segment.id;
        let mapping = MftMapping::new(geometry, segment.runs.try_copy()?)?;
        let boot_start = geometry
            .mft_lcn()
            .checked_mul(u64::from(geometry.cluster_size()))
            .ok_or_else(|| ScanError::overflow("record-zero offset"))?;
        if mapping.virtual_offset(boot_start)? != 0 {
            return Err(ScanError::bootstrap(
                "MFT first run disagrees with boot location",
            ));
        }
        let mut cursor = 0u64;
        let record_len = u64::from(geometry.file_record_size());
        while cursor < record_len {
            let remaining = usize::try_from(
                record_len
                    .checked_sub(cursor)
                    .ok_or_else(|| ScanError::overflow("record-zero remainder"))?,
            )
            .map_err(|_| ScanError::overflow("record-zero length"))?;
            let piece = mapping.read_piece(cursor, remaining)?;
            if boot_start.checked_add(cursor) != Some(piece.offset) {
                return Err(ScanError::bootstrap(
                    "record zero is not contiguous at the boot location",
                ));
            }
            cursor = cursor
                .checked_add(
                    u64::try_from(piece.len)
                        .map_err(|_| ScanError::overflow("record-zero piece"))?,
                )
                .ok_or_else(|| ScanError::overflow("record-zero cursor"))?;
        }
        Ok(Record0 {
            reference,
            data_id,
            data_size,
            mapping,
            attribute_lists: parts.lists,
            segments: parts.segments,
        })
    }
}
