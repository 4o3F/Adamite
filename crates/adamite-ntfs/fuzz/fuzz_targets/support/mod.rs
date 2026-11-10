#![expect(
    dead_code,
    reason = "each fuzz target uses only part of the shared decoder coverage"
)]

use adamite_ntfs::{
    Attribute, AttributeList, Bitmap, Data, FileName, IndexAllocation, NonResident, RecordHeader,
    ReparsePoint, Runlist, StandardInformation, Utf16Le, VcnRange, VolumeGeometry,
};
use std::hint::black_box;

pub fn name(value: Utf16Le<'_>) {
    black_box(value.len_units());
    for unit in value.units() {
        black_box(unit);
    }
    for character in value.decode_strict() {
        black_box(character).ok();
    }
    for character in value.decode_lossy() {
        black_box(character);
    }
    black_box(value.to_string_strict()).ok();
    black_box(value.to_string_lossy()).ok();
}
pub fn header(value: RecordHeader) {
    black_box(value.flags().contains(adamite_ntfs::RecordFlags::IN_USE));
    black_box(value.flags().contains(adamite_ntfs::RecordFlags::DIRECTORY));
    black_box((
        value.signature(),
        value.usa_offset(),
        value.usa_count(),
        value.lsn(),
        value.sequence(),
        value.hard_links(),
        value.first_attribute_offset(),
        value.flags().raw(),
        value.bytes_used(),
        value.bytes_allocated(),
        value.base_reference().raw(),
        value.next_attribute_id(),
        value.record_number(),
    ));
    let reference = value.base_reference();
    black_box((
        reference.record(),
        reference.sequence(),
        reference.is_null(),
        reference.to_le_bytes(),
    ));
}
pub fn geometry(value: VolumeGeometry) {
    black_box((
        value.bytes_per_sector(),
        value.sectors_per_cluster(),
        value.cluster_size(),
        value.file_record_size(),
        value.index_record_size(),
        value.total_sectors(),
        value.total_clusters(),
        value.total_bytes(),
        value.mft_lcn(),
        value.mftmirr_lcn(),
        value.serial(),
    ));
    for (lcn, count) in [(0, 1), (value.total_clusters(), 1), (u64::MAX, u64::MAX)] {
        black_box(value.cluster_byte_range(lcn, count)).ok();
    }
}
pub fn runs(mut value: Runlist<'_>) {
    let mut boot = [0u8; 512];
    boot[3..11].copy_from_slice(b"NTFS    ");
    boot[11..13].copy_from_slice(&512u16.to_le_bytes());
    boot[13] = 1;
    boot[40..48].copy_from_slice(&128u64.to_le_bytes());
    boot[48..56].copy_from_slice(&4u64.to_le_bytes());
    boot[56..64].copy_from_slice(&8u64.to_le_bytes());
    boot[64] = 0xf6;
    boot[68] = 0xf6;
    boot[510..].copy_from_slice(&[0x55, 0xaa]);
    let geometry = adamite_ntfs::parse_boot_sector(&boot).ok();
    for run in value.by_ref().flatten() {
        black_box((run.vcn(), run.length(), run.lcn()));
        if let Some(geometry) = geometry {
            black_box(run.validate_geometry(&geometry)).ok();
        }
    }
    black_box(value.next());
}
pub fn descriptor(value: NonResident<'_>) {
    black_box((
        value.lowest_vcn(),
        value.highest_vcn(),
        value.compression_unit(),
        value.mapping_pairs(),
    ));
    if let Some(sizes) = value.sizes() {
        black_box((
            sizes.allocated(),
            sizes.data(),
            sizes.initialized(),
            sizes.compressed(),
        ));
    }
    runs(value.runs());
    black_box(value.runs().collect_runs()).ok();
}
pub fn attribute(value: Attribute<'_>) {
    black_box(value.flags().contains(adamite_ntfs::AttributeFlags::SPARSE));
    black_box((
        value.as_bytes(),
        value.attribute_type().raw(),
        value.id(),
        value.flags().raw(),
        value.flags().compression_format(),
        value.is_resident(),
        value.resident_value(),
    ));
    name(value.name());
    if let Some(view) = value.non_resident() {
        descriptor(view);
    }
    if let Ok(info) = StandardInformation::from_attribute(value) {
        standard(info);
    }
    if let Ok(file) = FileName::from_attribute(value) {
        file_name(file);
    }
    if let Ok(data) = Data::from_attribute(value) {
        name(data.name());
        black_box((
            data.flags().raw(),
            data.resident_value(),
            data.logical_size(),
        ));
        if let Some(view) = data.non_resident() {
            descriptor(view);
        }
    }
    if let Ok(index) = IndexAllocation::from_attribute(value) {
        name(index.name());
        black_box(index.flags().raw());
        descriptor(index.descriptor());
    }
    if let Ok(point) = ReparsePoint::from_attribute(value) {
        black_box((point.tag(), point.data_length(), point.payload()));
    }
    if let Ok(list) = AttributeList::from_attribute(value) {
        list_entries(list);
    }
    if let Some(bytes) = value.resident_value() {
        raw_typed(bytes);
    }
}
pub fn standard(value: StandardInformation) {
    black_box(
        value
            .file_attributes()
            .contains(adamite_ntfs::FileAttributes::HIDDEN),
    );
    let times = value.times();
    black_box((
        times.created(),
        times.modified(),
        times.record_changed(),
        times.accessed(),
        value.file_attributes().raw(),
        value.owner_id(),
        value.security_id(),
        value.quota_charged(),
        value.usn(),
    ));
}
pub fn file_name(value: FileName<'_>) {
    black_box(
        value
            .file_attributes()
            .contains(adamite_ntfs::FileAttributes::DIRECTORY),
    );
    black_box((
        value.parent().record(),
        value.parent().sequence(),
        value.parent().is_null(),
        value.parent().to_le_bytes(),
    ));
    let times = value.times();
    black_box((
        value.parent().raw(),
        times.created(),
        times.modified(),
        times.record_changed(),
        times.accessed(),
        value.cached_allocated_size(),
        value.cached_data_size(),
        value.file_attributes().raw(),
        value.ea_size_or_reparse_tag(),
        value.namespace(),
    ));
    name(value.name());
}
pub fn list_entries(mut value: AttributeList<'_>) {
    for entry in value.by_ref().flatten() {
        black_box((
            entry.attribute_type().raw(),
            entry.lowest_vcn(),
            entry.segment_reference().raw(),
            entry.attribute_id(),
        ));
        name(entry.name());
    }
    black_box(value.next());
}
pub fn bitmap(value: Bitmap<'_>) {
    black_box(value.logical_bits());
    black_box(value.get(0));
    black_box(value.get(usize::MAX));
    for bit in value.iter() {
        black_box(bit);
    }
    for index in value.iter_set() {
        black_box(index);
    }
}
pub fn raw_typed(data: &[u8]) {
    if let Ok(value) = StandardInformation::parse(data) {
        standard(value);
    }
    if let Ok(value) = FileName::parse(data) {
        file_name(value);
    }
    if let Ok(value) = ReparsePoint::parse(data) {
        black_box((value.tag(), value.data_length(), value.payload()));
    }
    if let Ok(value) = AttributeList::new(data) {
        list_entries(value);
    }
    if let Ok(value) = Bitmap::new(data, data.len().saturating_mul(8)) {
        bitmap(value);
    }
    if let Ok(value) = Utf16Le::new(data) {
        name(value);
    }
}
pub fn vcn(data: &[u8]) -> Option<VcnRange> {
    let lowest = i64::from_le_bytes(data.get(..8)?.try_into().ok()?);
    let highest = i64::from_le_bytes(data.get(8..16)?.try_into().ok()?);
    let value = VcnRange::new(lowest, highest).ok()?;
    black_box((value.lowest_vcn(), value.highest_vcn(), value.is_empty()));
    Some(value)
}
