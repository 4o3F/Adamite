//! Merge invariants tested independently of transport.

use super::{
    AttributeFlags, AttributeType, BAD_CLUS, BAD_STREAM_NAME, EntryRecord, Merge, NodeFlags, Patch,
    ROOT, RecordContribution, RecordFlags, RecordSlots, ScanError, ScanStats, StreamSegment,
    StreamStorage,
};
use crate::mft::model::{NameAttribute, RecordKind, ReparseAttribute, StandardAttribute};
use crate::util::CounterExt;
use adamite_fixtures::builders::{AttrBuilder, NonResidentModel, RecordBuilder};
use adamite_fixtures::runlist_encoder::{RunModel, RunlistEncoder};
use adamite_ntfs::{
    record::{FileRecord, reference::FileRef},
    value::standard_information::StandardInformation,
};
use std::error::Error;

type TestResult = Result<(), Box<dyn Error>>;

#[test]
fn merge_reservation_prechecks_precede_allocation_and_preserve_contents() {
    let mut items = vec![7u8];
    assert!(matches!(
        Merge::reserve_contribution(&mut items, usize::MAX),
        Err(ScanError::Overflow {
            operation: "merged contribution length"
        })
    ));
    assert_eq!(items, [7]);
    assert!(matches!(
        Merge::reserve_contribution(&mut items, usize::MAX - 1),
        Err(ScanError::Allocation { .. })
    ));
    assert_eq!(items, [7]);
}

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
        flags: RecordFlags::from_raw(if record == ROOT { 3 } else { 1 }),
        conflicting_standard_information: false,
        names: vec![name(65)],
        standard_information: None,
        streams: Vec::new(),
        reparse_tags: Vec::new(),
    }
}
fn slots(base: RecordContribution) -> Result<RecordSlots, Box<dyn Error>> {
    slots_with_missing_base(base, None)
}
fn slots_with_missing_base(
    base: RecordContribution,
    missing: Option<usize>,
) -> Result<RecordSlots, Box<dyn Error>> {
    let index = usize::try_from(base.reference.record())?;
    let count = index.checked_add(1).ok_or("slot count")?.max(6);
    let mut slots = RecordSlots::new(count)?;
    let mut base = Some(base);
    for ordinal in 0..count {
        let contribution = if ordinal == index {
            base.take()
        } else if ordinal == 5 {
            Some(contribution(5))
        } else {
            None
        };
        let kind = if contribution.is_some() {
            RecordKind::Base
        } else {
            RecordKind::Unused
        };
        let contribution = if missing == Some(ordinal) {
            None
        } else {
            contribution
        };
        slots.push(kind, contribution)?;
    }
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
    let mut mapped_clusters = 0u64;
    for run in segment.runs() {
        let run = run?;
        if run.lcn().is_some() {
            mapped_clusters = mapped_clusters
                .checked_add(run.length())
                .ok_or("mapped clusters")?;
        }
        run_count.try_bump().map_err(ScanError::from)?;
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
            has_runs: run_count != 0,
            mapped_clusters,
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
    slots: RecordSlots,
    patches: Vec<Patch>,
) -> Result<(Vec<EntryRecord>, ScanStats), ScanError> {
    let mut merge = Merge::new(slots, patches, 512);
    let mut stats = ScanStats::default();
    for _ in 0..4096 {
        let before = (merge.patches.len(), merge.bases.len());
        let done = merge.step(&mut stats)?;
        let patches = before
            .0
            .checked_sub(merge.patches.len())
            .ok_or_else(|| ScanError::overflow("test patch progress"))?;
        // The handoff installs the base iterator without consuming it.
        let bases = before.1.saturating_sub(merge.bases.len());
        assert!(
            patches
                .checked_add(bases)
                .ok_or_else(|| ScanError::overflow("test batch size"))?
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
    added.add_standard(standard(6, 123)?);
    added
        .streams
        .push(nonresident(1, 9, model(1, 1, [-1; 4]), 0)?);
    let (entries, stats) = finish(slots(base)?, vec![patch(6, added)])?;
    let entry = find(&entries, 6)?;
    assert_eq!((entry.size, entry.ads_size, entry.alloc), (1024, 0, 1024));
    assert_eq!((entry.attrs, entry.mtime, entry.nlink), (6, 123, 3));
    assert_eq!(entry.names.len(), 2);
    assert_eq!(entry.names.get(1).ok_or("extension name")?.name, vec![66]);
    assert!(entry.flags.contains(NodeFlags::HIDDEN));
    assert!(entry.flags.contains(NodeFlags::SYSTEM));
    assert_eq!(stats.rejected_extensions, 0);
    Ok(())
}

#[test]
fn merge_missing_stale_and_discarded_bases_reject_patch_without_mutation() -> TestResult {
    for case in 0..5 {
        let mut record_slots = slots(contribution(6))?;
        let mut added = patch(6, contribution(1));
        added.contribution.names = vec![name(66)];
        match case {
            0 => added.base = reference(0x0000_ffff_ffff_ffff),
            1 => added.base = FileRef::from_raw((8u64 << 48) | 6),
            2 => {
                record_slots = slots(contribution(5))?;
                record_slots.push(RecordKind::Deleted, None)?;
            }
            3 => record_slots.base_mut(6).ok_or("slot")?.flags = RecordFlags::from_raw(0),
            _ => {
                record_slots = slots_with_missing_base(contribution(6), Some(6))?;
            }
        }
        let mut stats = ScanStats::default();
        let mut merge = Merge::new(record_slots, vec![], 512);
        merge.apply_patch(added, &mut stats)?;
        assert_eq!(stats.rejected_extensions, 1);
        assert_eq!(stats.invalid_file_refs, 1);
        if let Some(base) = merge.slots.base(6) {
            assert_eq!(base.names.len(), 1);
            assert_eq!(base.names.first().ok_or("base name")?.name, vec![65]);
            assert!(base.standard_information.is_none());
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
fn merge_unflagged_allocation_counts_mapped_runs_and_ignores_header_allocation() -> TestResult {
    for allocated in [512, 8192] {
        let mut base = contribution(6);
        let descriptor = NonResidentModel {
            mapping_pairs: RunlistEncoder {
                runs: vec![
                    RunModel {
                        lcn: Some(0),
                        len: 2,
                    },
                    RunModel { lcn: None, len: 3 },
                    RunModel {
                        lcn: Some(100),
                        len: 1,
                    },
                ],
            }
            .encode(0),
            ..model(0, 5, [allocated, 100, 50, 0])
        };
        base.streams.push(nonresident(6, 0, descriptor, 0)?);
        let (entries, stats) = finish(slots(base)?, vec![])?;
        let entry = find(&entries, 6)?;
        assert_eq!(
            (entry.size, entry.alloc),
            (100, 1536),
            "header allocated {allocated}"
        );
        assert_eq!(stats.corrupt_streams, 0);
    }
    Ok(())
}

#[test]
fn merge_unflagged_hole_only_and_empty_streams_allocate_zero() -> TestResult {
    for descriptor in [
        NonResidentModel {
            mapping_pairs: RunlistEncoder {
                runs: vec![RunModel { lcn: None, len: 8 }],
            }
            .encode(0),
            ..model(0, 7, [4096, 4096, 0, 0])
        },
        model(0, -1, [0; 4]),
    ] {
        let size = u64::try_from(descriptor.sizes[1])?;
        let mut base = contribution(6);
        base.streams.push(nonresident(6, 0, descriptor, 0)?);
        let (entries, stats) = finish(slots(base)?, vec![])?;
        assert_eq!(
            (find(&entries, 6)?.size, find(&entries, 6)?.alloc),
            (size, 0)
        );
        assert_eq!(stats.corrupt_streams, 0);
    }
    Ok(())
}

#[test]
fn merge_multiextent_holes_with_extension_segment_before_base() -> TestResult {
    let first = NonResidentModel {
        mapping_pairs: RunlistEncoder {
            runs: vec![
                RunModel {
                    lcn: Some(0),
                    len: 1,
                },
                RunModel { lcn: None, len: 2 },
            ],
        }
        .encode(0),
        ..model(0, 2, [4096, 3072, 0, 0])
    };
    let second = NonResidentModel {
        mapping_pairs: RunlistEncoder {
            runs: vec![
                RunModel { lcn: None, len: 1 },
                RunModel {
                    lcn: Some(100),
                    len: 2,
                },
            ],
        }
        .encode(0),
        ..model(3, 5, [-1; 4])
    };
    let mut base = contribution(6);
    // Deliberately put the continuation first in the unsorted stream list.
    base.streams.push(nonresident(1, 9, second, 0)?);
    base.streams.push(nonresident(6, 0, first, 0)?);
    let (entries, stats) = finish(slots(base)?, vec![])?;
    assert_eq!(
        (find(&entries, 6)?.size, find(&entries, 6)?.alloc),
        (3072, 1536)
    );
    assert_eq!(stats.corrupt_streams, 0);
    Ok(())
}

#[test]
fn merge_compressed_padding_runs_keep_compressed_header_allocation() -> TestResult {
    let descriptor = NonResidentModel {
        compression_unit: 4,
        mapping_pairs: RunlistEncoder {
            runs: vec![
                RunModel {
                    lcn: Some(100),
                    len: 3,
                },
                RunModel { lcn: None, len: 13 },
            ],
        }
        .encode(0),
        ..model(0, 15, [8192, 8000, 8000, 1536])
    };
    let mut base = contribution(6);
    base.streams.push(nonresident(6, 0, descriptor, 1)?);
    let (entries, stats) = finish(slots(base)?, vec![])?;
    let entry = find(&entries, 6)?;
    assert_eq!((entry.size, entry.alloc), (8000, 1536));
    assert!(entry.flags.contains(NodeFlags::COMPRESSED));
    assert_eq!(stats.corrupt_streams, 0);
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
        assert_eq!(entry.flags.contains(NodeFlags::SPARSE), flags == 0x8000);
        assert_eq!(entry.flags.contains(NodeFlags::COMPRESSED), flags == 1);
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
    for record in [6, BAD_CLUS] {
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
        assert_eq!(entry.ads_size, if record == BAD_CLUS { 5 } else { 4101 });
    }
    Ok(())
}

#[test]
fn merge_reparse_directory_and_si_flags_are_retained() -> TestResult {
    let mut base = contribution(6);
    base.add_standard(standard(0x10 | 0x400 | 0x200 | 0x800 | 6, 456)?);
    base.reparse_tags.push(ReparseAttribute { tag: 0xa000000c });
    let (entries, _) = finish(slots(base)?, vec![])?;
    let entry = find(&entries, 6)?;
    for flag in [
        NodeFlags::DIRECTORY,
        NodeFlags::REPARSE,
        NodeFlags::SPARSE,
        NodeFlags::COMPRESSED,
        NodeFlags::HIDDEN,
        NodeFlags::SYSTEM,
    ] {
        assert!(entry.flags.contains(flag));
    }
    assert_eq!(entry.reparse_tag, Some(0xa000000c));
    assert_eq!(entry.mtime, 456);
    Ok(())
}

#[test]
fn merge_missing_root_is_fatal() -> TestResult {
    let without_root = slots_with_missing_base(contribution(6), Some(5))?;
    for slots in [RecordSlots::default(), without_root] {
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
    base.add_standard(standard(0, 1)?);
    base.add_standard(standard(0, 2)?);
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
    let clusters = 1i64 << 55;
    base.streams.push(nonresident(
        6,
        0,
        model(0, clusters - 1, [512, 1, 1, 0]),
        0,
    )?);
    assert!(matches!(
        finish(slots(base)?, vec![]),
        Err(ScanError::Overflow {
            operation: "merged stream mapped bytes"
        })
    ));

    let mut base = contribution(6);
    let clusters = 1i64 << 54;
    for id in 0..2 {
        let mut segment = nonresident(6, id, model(0, clusters - 1, [512, 1, 1, 0]), 0)?;
        segment.name = vec![id];
        base.streams.push(segment);
    }
    assert!(matches!(
        finish(slots(base)?, vec![]),
        Err(ScanError::Overflow {
            operation: "entry allocation"
        })
    ));
    Ok(())
}

#[test]
fn merge_cpu_steps_handle_at_most_256_patches_or_slots() -> TestResult {
    let mut slots = RecordSlots::new(700)?;
    for ordinal in 0..700 {
        slots.push(RecordKind::Base, Some(contribution(ordinal)))?;
    }
    let patches = (700..1000)
        .map(|ordinal| patch(6, contribution(ordinal)))
        .collect();
    let (entries, _) = finish(slots, patches)?;
    assert_eq!(entries.len(), 700);
    assert_eq!(find(&entries, 6)?.names.len(), 301);
    Ok(())
}

#[test]
fn merge_inline_standard_information_preserves_base_and_extension_rules() -> TestResult {
    for (base_times, extension_times, expected) in [
        (vec![], vec![], Some(0)),
        (vec![123], vec![], Some(123)),
        (vec![], vec![123, 123], Some(123)),
        (vec![123, 123], vec![123], Some(123)),
        (vec![123], vec![456], None),
        (vec![], vec![123, 456], None),
        (vec![123, 456], vec![123], None),
    ] {
        let mut base = contribution(6);
        for time in base_times {
            base.add_standard(standard(0, time)?);
        }
        let mut patches = Vec::new();
        for (index, time) in extension_times.into_iter().enumerate() {
            let mut added = contribution(u64::try_from(index)? + 10);
            added.add_standard(standard(0, time)?);
            patches.push(patch(6, added));
        }
        let (entries, stats) = finish(slots(base)?, patches)?;
        if let Some(time) = expected {
            assert_eq!(find(&entries, 6)?.mtime, time);
            assert_eq!(stats.corrupt_values, 0);
            assert_eq!(stats.corrupt_records, 0);
        } else {
            assert!(entries.iter().all(|entry| entry.file_ref.record() != 6));
            assert_eq!(stats.corrupt_values, 1);
            assert_eq!(stats.corrupt_records, 1);
        }
    }
    // Conflicts inside a single accepted extension must also reach the base.
    let mut added = contribution(10);
    added.add_standard(standard(0, 123)?);
    added.add_standard(standard(0, 456)?);
    let (entries, stats) = finish(slots(contribution(6))?, vec![patch(6, added)])?;
    assert!(entries.iter().all(|entry| entry.file_ref.record() != 6));
    assert_eq!((stats.corrupt_values, stats.corrupt_records), (1, 1));
    Ok(())
}
