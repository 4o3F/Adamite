//! Validated record-zero and extension contributions for MFT discovery.

use super::{
    mapping::{MftMapping, OwnedRun, overflow},
    scanner::ScanError,
};
use adamite_ntfs::{
    Attribute, AttributeFlags, AttributeType, FileRecord, FileRef, NonResident, RecordFlags,
    StreamSizes, VolumeGeometry, fixup,
};

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
pub(super) struct DataSegment {
    pub id: u16,
    pub lowest_vcn: u64,
    pub sizes: Option<StreamSizes>,
    pub runs: Vec<OwnedRun>,
}

#[derive(Debug)]
pub(super) struct RecordParts {
    pub reference: FileRef,
    pub segments: Vec<DataSegment>,
    pub lists: Vec<AttributeListDescriptor>,
}

#[derive(Debug)]
pub(super) struct Record0 {
    pub reference: FileRef,
    pub data_id: u16,
    pub data_size: u64,
    pub mapping: MftMapping,
    pub attribute_lists: Vec<AttributeListDescriptor>,
    pub segments: Vec<DataSegment>,
}

pub(super) fn copy_slice<T: Copy>(input: &[T]) -> Result<Vec<T>, ScanError> {
    let mut owned = Vec::new();
    owned
        .try_reserve_exact(input.len())
        .map_err(|source| ScanError::Allocation { source })?;
    owned.extend_from_slice(input);
    Ok(owned)
}

pub(super) fn push<T>(items: &mut Vec<T>, item: T) -> Result<(), ScanError> {
    items
        .try_reserve(1)
        .map_err(|source| ScanError::Allocation { source })?;
    items.push(item);
    Ok(())
}

pub(super) fn unsupported(reason: &'static str) -> ScanError {
    ScanError::Bootstrap { reason }
}

pub(super) fn check_storage(flags: AttributeFlags, compression_unit: u8) -> Result<(), ScanError> {
    if flags.compression_format() != 0
        || flags.contains(AttributeFlags::SPARSE)
        || flags.contains(AttributeFlags::ENCRYPTED)
        || compression_unit != 0
    {
        return Err(unsupported(
            "sparse, compressed or encrypted bootstrap storage",
        ));
    }
    Ok(())
}

pub(super) fn collect_runs(
    segment: NonResident<'_>,
    geometry: VolumeGeometry,
) -> Result<MftMapping, ScanError> {
    collect_mapping(segment.runs(), geometry)
}

pub(super) fn collect_mapping(
    runs: adamite_ntfs::Runlist<'_>,
    geometry: VolumeGeometry,
) -> Result<MftMapping, ScanError> {
    let mut owned = Vec::new();
    // Exhaust the iterator so its terminator and VCN coverage are validated too.
    for run in runs {
        let run = run.map_err(|source| ScanError::Runlist { source })?;
        let lcn = run
            .lcn()
            .ok_or_else(|| unsupported("sparse bootstrap run"))?;
        push(
            &mut owned,
            OwnedRun {
                vcn: run.vcn(),
                lcn,
                clusters: run.length(),
            },
        )?;
    }
    MftMapping::new(geometry, owned)
}

fn list_descriptor(attribute: Attribute<'_>) -> Result<AttributeListDescriptor, ScanError> {
    let storage = match attribute.resident_value() {
        Some(value) => ListStorage::Resident(copy_slice(value)?),
        None => {
            let segment = attribute
                .non_resident()
                .ok_or_else(|| unsupported("missing attribute-list descriptor"))?;
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
    Ok(AttributeListDescriptor {
        id: attribute.id(),
        name,
        flags: attribute.flags(),
        storage,
    })
}

fn parts(
    record: &FileRecord<'_>,
    reference: FileRef,
    geometry: VolumeGeometry,
) -> Result<RecordParts, ScanError> {
    let mut segments = Vec::new();
    let mut lists = Vec::new();
    for attribute in record.attributes() {
        let attribute = attribute.map_err(|source| ScanError::Attribute { source })?;
        if attribute.attribute_type() == AttributeType::DATA && attribute.name().len_units() == 0 {
            let segment = attribute
                .non_resident()
                .ok_or_else(|| unsupported("resident MFT DATA"))?;
            check_storage(attribute.flags(), segment.compression_unit())?;
            let mapping = collect_runs(segment, geometry)?;
            push(
                &mut segments,
                DataSegment {
                    id: attribute.id(),
                    lowest_vcn: segment.lowest_vcn(),
                    sizes: segment.sizes(),
                    runs: mapping.runs,
                },
            )?;
        } else if attribute.attribute_type() == AttributeType::ATTRIBUTE_LIST {
            push(&mut lists, list_descriptor(attribute)?)?;
        }
    }
    Ok(RecordParts {
        reference,
        segments,
        lists,
    })
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
        return Err(unsupported(
            "record zero must be an in-use base with its own identity and nonzero sequence",
        ));
    }
    let reference = FileRef::from_raw(u64::from(header.sequence()) << 48);
    let parts = parts(&record, reference, geometry)?;
    let mut first = parts
        .segments
        .iter()
        .filter(|segment| segment.lowest_vcn == 0);
    let segment = first
        .next()
        .ok_or_else(|| unsupported("missing unnamed MFT DATA first segment"))?;
    if first.next().is_some() {
        return Err(unsupported("duplicate first MFT DATA segment"));
    }
    let data_size = segment
        .sizes
        .ok_or_else(|| unsupported("missing MFT stream sizes"))?
        .data();
    if data_size == 0 || !data_size.is_multiple_of(u64::from(geometry.file_record_size())) {
        return Err(unsupported(
            "MFT data size must be positive and record aligned",
        ));
    }
    let data_id = segment.id;
    let mapping = MftMapping::new(geometry, copy_slice(&segment.runs)?)?;
    let boot_start = geometry
        .mft_lcn()
        .checked_mul(u64::from(geometry.cluster_size()))
        .ok_or_else(|| overflow("record-zero offset"))?;
    if mapping.virtual_offset(boot_start)? != 0 {
        return Err(unsupported("MFT first run disagrees with boot location"));
    }
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
            return Err(unsupported(
                "record zero is not contiguous at the boot location",
            ));
        }
        cursor = cursor
            .checked_add(u64::try_from(piece.len).map_err(|_| overflow("record-zero piece"))?)
            .ok_or_else(|| overflow("record-zero cursor"))?;
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

pub(super) fn parse_extension(
    mut bytes: Vec<u8>,
    reference: FileRef,
    base: FileRef,
    geometry: VolumeGeometry,
) -> Result<RecordParts, ScanError> {
    let fixed = fixup::apply(&mut bytes).map_err(|source| ScanError::Fixup { source })?;
    let record = FileRecord::parse(fixed).map_err(|source| ScanError::Record { source })?;
    let header = record.header();
    if !header.flags().contains(RecordFlags::IN_USE)
        || header.sequence() != reference.sequence()
        || header.base_reference() != base
        || header
            .record_number()
            .is_some_and(|ordinal| u64::from(ordinal) != reference.record())
    {
        return Err(unsupported(
            "stale or invalid MFT extension identity or base reference",
        ));
    }
    parts(&record, reference, geometry)
}
