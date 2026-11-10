//! Merge invariants tested independently of transport.

use super::*;
use crate::entry::{NameAttribute, ReparseAttribute, StandardAttribute};
use adamite_fixtures::builders::{AttrBuilder, NonResidentModel, RecordBuilder};
use adamite_fixtures::runlist_encoder::{RunModel, RunlistEncoder};
use adamite_ntfs::{FileRecord, FileRef, StandardInformation};
use std::error::Error;

type TestResult = Result<(), Box<dyn Error>>;

fn reference(record: u64) -> FileRef {
    FileRef::from_raw((7u64 << 48) | record)
}
fn name(value: u16) -> NameAttribute {
    NameAttribute {
        parent: reference(5),
        namespace: 1,
        name: vec![value],
    }
}
fn contribution(record: u64) -> RecordContribution {
    RecordContribution {
        reference: reference(record),
        hard_links: 1,
        names: vec![name(65)],
        standard_information: Vec::new(),
        streams: Vec::new(),
        reparse_tags: Vec::new(),
    }
}
fn empty_slot() -> RecordSlot {
    RecordSlot {
        classification: RecordKind::Unused,
        sequence: None,
        flags: None,
        base: None,
    }
}
fn base_slot(base: RecordContribution) -> RecordSlot {
    let flags = if base.reference.record() == ROOT_RECORD {
        3
    } else {
        1
    };
    RecordSlot {
        classification: RecordKind::Base,
        sequence: Some(base.reference.sequence()),
        flags: Some(RecordFlags::from_raw(flags)),
        base: Some(base),
    }
}
fn slots(base: RecordContribution) -> Result<Vec<RecordSlot>, Box<dyn Error>> {
    let count = usize::try_from(base.reference.record())?
        .checked_add(1)
        .ok_or("slot count")?
        .max(6);
    let mut slots: Vec<_> = (0..count).map(|_| empty_slot()).collect();
    *slots.get_mut(5).ok_or("root slot")? = base_slot(contribution(5));
    let index = usize::try_from(base.reference.record())?;
    *slots.get_mut(index).ok_or("base slot")? = base_slot(base);
    Ok(slots)
}
fn patch(base: u64, added: RecordContribution) -> Patch {
    Patch {
        base: reference(base),
        source: added.reference,
        contribution: added,
    }
}

fn resident(source: u64, id: u16, name: Vec<u16>, size: u64) -> StreamSegment {
    StreamSegment {
        source: reference(source),
        attribute_type: AttributeType::DATA,
        name,
        attribute_id: id,
        flags: AttributeFlags::from_raw(0),
        storage: StreamStorage::Resident { size },
    }
}

fn nonresident(
    source: u64,
    id: u16,
    model: NonResidentModel,
    flags: u16,
) -> Result<StreamSegment, Box<dyn Error>> {
    let mut attr = AttrBuilder::non_resident(0x80, model);
    attr.flags = flags;
    let mut raw = RecordBuilder {
        attributes: vec![attr.build()],
        ..RecordBuilder::default()
    }
    .build();
    let record = FileRecord::parse(&mut raw)?;
    let attr = record.attributes().next().ok_or("test attribute")??;
    let segment = attr.non_resident().ok_or("test segment")?;
    let mut run_count = 0u64;
    for run in segment.runs() {
        run?;
        bump(&mut run_count)?;
    }
    Ok(StreamSegment {
        source: reference(source),
        attribute_type: AttributeType::DATA,
        name: Vec::new(),
        attribute_id: id,
        flags: attr.flags(),
        storage: StreamStorage::NonResident {
            lowest_vcn: segment.lowest_vcn(),
            highest_vcn: segment.highest_vcn(),
            compression_unit: segment.compression_unit(),
            sizes: segment.sizes(),
            run_count,
        },
    })
}

fn model(lowest: i64, highest: i64, sizes: [i64; 4]) -> NonResidentModel {
    let length = u64::try_from(
        highest
            .checked_sub(lowest)
            .and_then(|n| n.checked_add(1))
            .unwrap_or(0),
    )
    .unwrap_or(0);
    NonResidentModel {
        lowest,
        highest,
        sizes,
        mapping_pairs: RunlistEncoder {
            runs: if length == 0 {
                Vec::new()
            } else {
                vec![RunModel {
                    lcn: Some(100),
                    len: length,
                }]
            },
        }
        .encode(0),
        ..NonResidentModel::default()
    }
}

fn standard(attrs: u32, mtime: u64) -> Result<StandardAttribute, Box<dyn Error>> {
    let mut bytes = [0u8; 48];
    bytes
        .get_mut(8..16)
        .ok_or("SI time")?
        .copy_from_slice(&mtime.to_le_bytes());
    bytes
        .get_mut(32..36)
        .ok_or("SI attrs")?
        .copy_from_slice(&attrs.to_le_bytes());
    Ok(StandardAttribute {
        value: StandardInformation::parse(&bytes)?,
    })
}

fn finish(
    slots: Vec<RecordSlot>,
    patches: Vec<Patch>,
) -> Result<(Vec<EntryRecord>, ScanStats), ScanError> {
    let mut merge = Merge::new(slots, patches, 512);
    let mut stats = ScanStats::default();
    for _ in 0..4096 {
        let before = (merge.patches.len(), merge.cursor);
        let done = merge.step(&mut stats)?;
        let patches = before
            .0
            .checked_sub(merge.patches.len())
            .ok_or_else(|| overflow("test patch progress"))?;
        let slots = merge
            .cursor
            .checked_sub(before.1)
            .ok_or_else(|| overflow("test slot progress"))?;
        assert!(
            patches
                .checked_add(slots)
                .ok_or_else(|| overflow("test batch size"))?
                <= 256
        );
        if done {
            return Ok((merge.entries, stats));
        }
    }
    Err(ScanError::Stream {
        reason: "merge test did not terminate",
    })
}
fn find(entries: &[EntryRecord], record: u64) -> Result<&EntryRecord, Box<dyn Error>> {
    entries
        .iter()
        .find(|entry| entry.file_ref.record() == record)
        .ok_or_else(|| format!("missing entry {record}").into())
}

#[test]
fn merge_extension_before_base_and_multisegment_stream_counted_once() -> TestResult {
    let mut base = contribution(6);
    base.hard_links = 3;
    base.streams
        .push(nonresident(6, 1, model(0, 0, [1024, 1024, 1024, 0]), 0)?);
    let mut added = contribution(1);
    added.hard_links = 9;
    added.names = vec![name(66)];
    added.standard_information.push(standard(6, 123)?);
    added
        .streams
        .push(nonresident(1, 9, model(1, 1, [-1; 4]), 0)?);
    let (entries, stats) = finish(slots(base)?, vec![patch(6, added)])?;
    let entry = find(&entries, 6)?;
    assert_eq!((entry.size, entry.ads_size, entry.alloc), (1024, 0, 1024));
    assert_eq!((entry.attrs, entry.mtime, entry.nlink), (6, 123, 3));
    assert_eq!(entry.names.len(), 2);
    assert_eq!(entry.names.get(1).ok_or("extension name")?.name, vec![66]);
    assert!(entry.flags.contains(EntryFlags::HIDDEN));
    assert!(entry.flags.contains(EntryFlags::SYSTEM));
    assert_eq!(stats.rejected_extensions, 0);
    Ok(())
}

#[test]
fn merge_missing_stale_and_discarded_bases_reject_patch_without_mutation() -> TestResult {
    for case in 0..5 {
        let mut slots = slots(contribution(6))?;
        let mut added = patch(6, contribution(1));
        added.contribution.names = vec![name(66)];
        match case {
            0 => added.base = reference(0x0000_ffff_ffff_ffff),
            1 => added.base = FileRef::from_raw((8u64 << 48) | 6),
            2 => slots.get_mut(6).ok_or("slot")?.classification = RecordKind::Deleted,
            3 => slots.get_mut(6).ok_or("slot")?.flags = Some(RecordFlags::from_raw(0)),
            _ => slots.get_mut(6).ok_or("slot")?.base = None,
        }
        let mut stats = ScanStats::default();
        apply_patch(&mut slots, added, &mut stats)?;
        assert_eq!(stats.rejected_extensions, 1);
        assert_eq!(stats.invalid_file_refs, 1);
        if let Some(base) = slots.get(6).and_then(|slot| slot.base.as_ref()) {
            assert_eq!(base.names.len(), 1);
            assert_eq!(base.names.first().ok_or("base name")?.name, vec![65]);
            assert!(base.standard_information.is_empty());
            assert!(base.streams.is_empty());
        }
    }
    Ok(())
}

#[test]
fn merge_duplicate_first_segment_discards_base_and_records_diagnostic() -> TestResult {
    let mut base = contribution(6);
    base.streams
        .push(nonresident(6, 0, model(0, 0, [512, 512, 512, 0]), 0)?);
    let mut added = contribution(1);
    added
        .streams
        .push(nonresident(1, 1, model(0, 0, [512, 512, 512, 0]), 0)?);
    let (entries, stats) = finish(slots(base)?, vec![patch(6, added)])?;
    assert!(entries.iter().all(|entry| entry.file_ref.record() != 6));
    assert_eq!(stats.corrupt_streams, 1);
    assert_eq!(stats.corrupt_records, 1);
    assert_eq!(stats.rejected_extensions, 0);
    Ok(())
}

#[test]
fn merge_overlap_gap_missing_first_and_truncated_coverage_discard_stream() -> TestResult {
    for (lowest, highest) in [(1, 2), (3, 3)] {
        let mut base = contribution(6);
        base.streams
            .push(nonresident(6, 0, model(0, 1, [1536, 1536, 1536, 0]), 0)?);
        let mut added = contribution(1);
        added
            .streams
            .push(nonresident(1, 1, model(lowest, highest, [-1; 4]), 0)?);
        let (entries, stats) = finish(slots(base)?, vec![patch(6, added)])?;
        assert!(entries.iter().all(|entry| entry.file_ref.record() != 6));
        assert_eq!(stats.corrupt_streams, 1);
    }
    for descriptor in [model(1, 1, [-1; 4]), model(0, 0, [1024, 1024, 0, 0])] {
        let mut base = contribution(6);
        base.streams.push(nonresident(6, 0, descriptor, 0)?);
        let (entries, stats) = finish(slots(base)?, vec![])?;
        assert!(entries.iter().all(|entry| entry.file_ref.record() != 6));
        assert_eq!(stats.corrupt_streams, 1);
    }
    Ok(())
}

#[test]
fn merge_sparse_and_compressed_use_first_segment_compressed_size() -> TestResult {
    for flags in [0x8000, 1] {
        let mut base = contribution(6);
        let first = NonResidentModel {
            compression_unit: 4,
            ..model(0, 3, [4096, 3000, 3000, 512])
        };
        let second = NonResidentModel {
            compression_unit: 4,
            ..model(4, 7, [-1; 4])
        };
        base.streams.push(nonresident(6, 0, first, flags)?);
        let mut added = contribution(1);
        added.streams.push(nonresident(1, 1, second, flags)?);
        let (entries, _) = finish(slots(base)?, vec![patch(6, added)])?;
        let entry = find(&entries, 6)?;
        assert_eq!((entry.size, entry.alloc), (3000, 512));
        assert_eq!(entry.flags.contains(EntryFlags::SPARSE), flags == 0x8000);
        assert_eq!(entry.flags.contains(EntryFlags::COMPRESSED), flags == 1);
    }
    Ok(())
}

#[test]
fn merge_resident_ads_index_and_bitmap_accounting_uses_stream_identity() -> TestResult {
    let mut base = contribution(6);
    base.streams = vec![
        resident(6, 0, vec![], 3),
        resident(6, 1, vec![65], 2),
        resident(6, 2, vec![97], 5),
    ];
    let mut index = nonresident(6, 3, model(0, 0, [512, 300, 300, 0]), 0)?;
    index.attribute_type = AttributeType::INDEX_ALLOCATION;
    index.name = vec![65];
    let mut bitmap = nonresident(6, 4, model(0, 0, [512, 100, 100, 0]), 0)?;
    bitmap.attribute_type = AttributeType::BITMAP;
    base.streams.extend([index, bitmap]);
    let (entries, _) = finish(slots(base)?, vec![])?;
    let entry = find(&entries, 6)?;
    assert_eq!((entry.size, entry.ads_size, entry.alloc), (3, 7, 1024));
    let mut resident_only = contribution(6);
    resident_only.streams.push(resident(6, 0, vec![], 9000));
    let (entries, _) = finish(slots(resident_only)?, vec![])?;
    assert_eq!(find(&entries, 6)?.alloc, 0);
    Ok(())
}

#[test]
fn merge_duplicate_resident_or_mixed_storage_discards_base() -> TestResult {
    for extra in [
        resident(1, 1, vec![], 3),
        nonresident(1, 1, model(0, 0, [512, 3, 3, 0]), 0)?,
    ] {
        let mut base = contribution(6);
        base.streams.push(resident(6, 0, vec![], 3));
        let mut added = contribution(1);
        added.streams.push(extra);
        let (entries, stats) = finish(slots(base)?, vec![patch(6, added)])?;
        assert!(entries.iter().all(|entry| entry.file_ref.record() != 6));
        assert_eq!(stats.corrupt_streams, 1);
    }
    Ok(())
}

#[test]
fn merge_badclus_bad_excludes_only_its_logical_ads_size() -> TestResult {
    for record in [6, BAD_CLUS_RECORD] {
        let mut base = contribution(record);
        base.streams.push(resident(record, 0, vec![], 0));
        let mut bad = nonresident(record, 1, model(0, 7, [4096, 4096, 0, 0]), 0)?;
        bad.name = BAD_STREAM_NAME.to_vec();
        base.streams.push(bad);
        base.streams.push(resident(record, 2, vec![88], 5));
        let (entries, _) = finish(slots(base)?, vec![])?;
        let entry = find(&entries, record)?;
        assert_eq!(entry.size, 0);
        assert_eq!(entry.alloc, 4096);
        assert_eq!(
            entry.ads_size,
            if record == BAD_CLUS_RECORD { 5 } else { 4101 }
        );
    }
    Ok(())
}

#[test]
fn merge_reparse_directory_and_si_flags_are_retained() -> TestResult {
    let mut base = contribution(6);
    base.standard_information
        .push(standard(0x10 | 0x400 | 0x200 | 0x800 | 6, 456)?);
    base.reparse_tags.push(ReparseAttribute { tag: 0xa000000c });
    let (entries, _) = finish(slots(base)?, vec![])?;
    let entry = find(&entries, 6)?;
    for flag in [
        EntryFlags::DIRECTORY,
        EntryFlags::REPARSE,
        EntryFlags::SPARSE,
        EntryFlags::COMPRESSED,
        EntryFlags::HIDDEN,
        EntryFlags::SYSTEM,
    ] {
        assert!(entry.flags.contains(flag));
    }
    assert_eq!(entry.reparse_tag, Some(0xa000000c));
    assert_eq!(entry.mtime, 456);
    Ok(())
}

#[test]
fn merge_missing_root_is_fatal() -> TestResult {
    let mut without_root = slots(contribution(6))?;
    *without_root.get_mut(5).ok_or("root slot")? = empty_slot();
    for slots in [Vec::new(), without_root] {
        assert!(matches!(
            finish(slots, vec![]),
            Err(ScanError::RequiredRecord { record: 5 })
        ));
    }
    Ok(())
}

#[test]
fn merge_conflicting_si_and_corrupt_root_are_fatal_or_discarded() -> TestResult {
    let mut base = contribution(6);
    base.standard_information = vec![standard(0, 1)?, standard(0, 2)?];
    let (entries, stats) = finish(slots(base)?, vec![])?;
    assert!(entries.iter().all(|entry| entry.file_ref.record() != 6));
    assert_eq!(stats.corrupt_values, 1);
    assert_eq!(stats.corrupt_records, 1);
    let mut root = contribution(5);
    root.streams = vec![resident(5, 1, vec![], 1), resident(5, 2, vec![], 2)];
    assert!(matches!(
        finish(slots(root)?, vec![]),
        Err(ScanError::RequiredRecord { record: 5 })
    ));
    Ok(())
}

#[test]
fn merge_allocation_overflow_is_fatal() -> TestResult {
    let mut base = contribution(6);
    for id in 0..3 {
        let mut segment = nonresident(6, id, model(0, 0, [i64::MAX, 1, 1, 0]), 0)?;
        segment.name = vec![id];
        base.streams.push(segment);
    }
    assert!(matches!(
        finish(slots(base)?, vec![]),
        Err(ScanError::Overflow { .. })
    ));
    Ok(())
}

#[test]
fn merge_cpu_steps_handle_at_most_256_patches_or_slots() -> TestResult {
    let slots = (0..700)
        .map(|ordinal| base_slot(contribution(ordinal)))
        .collect();
    let patches = (700..1000)
        .map(|ordinal| patch(6, contribution(ordinal)))
        .collect();
    let (entries, _) = finish(slots, patches)?;
    assert_eq!(entries.len(), 700);
    assert_eq!(find(&entries, 6)?.names.len(), 301);
    Ok(())
}
