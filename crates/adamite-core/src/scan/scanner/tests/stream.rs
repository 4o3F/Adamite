//! Stream phase regressions.

use super::support::{
    DriverOutcome, TestResult, drive_to, drive_to_observed, ingest_volume, named_record, put,
};
use crate::error::ScanError;
use crate::mft::model::{RecordKind, StreamStorage};
use crate::scan::{ScanConfig, ScanPhase, ScanStep, Scanner};
use adamite_fixtures::Fixture;
use adamite_fixtures::builders::{AttrBuilder, FileNameBuilder, NonResidentModel, RecordBuilder};
use adamite_fixtures::runlist_encoder::{RunModel, RunlistEncoder};
use adamite_ntfs::attribute::AttributeType;
use std::error::Error;
use std::path::Path;

fn ingest_image(image: &[u8], cap: usize, chunk: usize) -> Result<DriverOutcome, Box<dyn Error>> {
    let mut scanner = Scanner::new(ScanConfig {
        max_outstanding: cap,
        chunk_size: chunk,
        ..ScanConfig::default()
    })?;
    let (requests, outcome) = drive_to(&mut scanner, image, ScanPhase::Merge)?;
    Ok((scanner, requests, outcome))
}

#[test]
fn ingest_classification_and_patches_do_not_double_count_bootstrap() -> TestResult {
    let mut baad = vec![0; 1024];
    baad.get_mut(..4)
        .ok_or("BAAD signature")?
        .copy_from_slice(b"BAAD");
    let deleted = RecordBuilder {
        flags: 0,
        record_number: 4,
        ..RecordBuilder::default()
    };
    let mut deleted_bytes = deleted.build();
    deleted.corrupt_tail(&mut deleted_bytes, 0);
    let patch = RecordBuilder {
        base: (7u64 << 48) | 6,
        ..named_record(7, vec![80])
    };
    let mut torn = named_record(8, vec![84]).build();
    RecordBuilder::default().corrupt_tail(&mut torn, 0);
    let volume = ingest_volume(
        16,
        vec![
            (2, vec![0xff; 1024]),
            (3, baad),
            (4, deleted_bytes),
            (6, named_record(6, vec![70]).build()),
            (7, patch.build()),
            (8, torn),
        ],
    )?;
    let (scanner, _, outcome) = ingest_image(&volume, 3, 512)?;
    outcome?;
    let state = scanner.ingest.as_ref().ok_or("ingestion handoff")?;
    assert_eq!(state.slots.len(), 16);
    assert_eq!(state.stats.records_total, 16);
    assert_eq!(state.stats.records_in_use, 4);
    assert_eq!(state.stats.base_records, 3);
    assert_eq!(state.stats.extension_records, 1);
    assert_eq!(state.stats.unused, 9);
    assert_eq!(state.stats.deleted, 1);
    assert_eq!(state.stats.baad, 1);
    assert_eq!(state.stats.corrupt_records, 1);
    assert_eq!(state.stats.bad_fixup, 1);
    for (ordinal, kind) in [
        (1, RecordKind::Unused),
        (2, RecordKind::Unused),
        (3, RecordKind::Baad),
        (4, RecordKind::Deleted),
        (6, RecordKind::Base),
        (7, RecordKind::Extension),
        (8, RecordKind::Corrupt),
    ] {
        assert_eq!(state.slots.get(ordinal).ok_or("slot")?.classification, kind);
    }
    let patch = state.patches.first().ok_or("extension patch")?;
    assert_eq!(
        patch.base,
        adamite_ntfs::record::reference::FileRef::from_raw((7u64 << 48) | 6)
    );
    assert_eq!(patch.source.record(), 7);
    assert_eq!(patch.contribution.reference, patch.source);
    let name = patch.contribution.names.first().ok_or("patch name")?;
    assert_eq!(
        name.parent,
        adamite_ntfs::record::reference::FileRef::from_raw((7u64 << 48) | 5)
    );
    assert_eq!(name.namespace, 1);
    assert_eq!(
        patch.contribution.names.first().ok_or("patch name")?.name,
        vec![80]
    );
    assert!(state.slots.base(7).is_none());
    assert_eq!(scanner.phase, ScanPhase::Merge);
    Ok(())
}

#[test]
fn ingest_late_attribute_and_runlist_errors_discard_whole_contribution() -> TestResult {
    let good_name = AttrBuilder::resident(0x30, FileNameBuilder::default().build()).build();
    let mut invalid = AttrBuilder::resident(0x10, vec![]).build();
    invalid.get_mut(..4).ok_or("invalid attribute")?.fill(0);
    let runlist = AttrBuilder::non_resident(
        0x80,
        NonResidentModel {
            highest: 1,
            mapping_pairs: vec![0x11, 1, 100, 0],
            ..NonResidentModel::default()
        },
    );
    let bad_base = RecordBuilder {
        record_number: 6,
        attributes: vec![good_name.clone(), invalid],
        ..RecordBuilder::default()
    };
    let bad_extension = RecordBuilder {
        record_number: 7,
        base: (7u64 << 48) | 6,
        attributes: vec![good_name, runlist.build()],
        ..RecordBuilder::default()
    };
    let volume = ingest_volume(16, vec![(6, bad_base.build()), (7, bad_extension.build())])?;
    let (scanner, _, outcome) = ingest_image(&volume, 4, 4096)?;
    outcome?;
    let state = scanner.ingest.as_ref().ok_or("state")?;
    assert!(state.slots.base(6).is_none());
    assert!(state.patches.is_empty());
    assert_eq!(state.stats.corrupt_records, 2);
    assert_eq!(state.stats.corrupt_attributes, 1);
    assert_eq!(state.stats.corrupt_runlists, 1);
    assert_eq!(state.stats.base_records, 3);
    assert_eq!(state.stats.extension_records, 1);
    Ok(())
}

#[test]
fn ingest_record_number_mismatch_is_rejected() -> TestResult {
    let volume = ingest_volume(16, vec![(6, named_record(99, vec![88]).build())])?;
    let (scanner, _, outcome) = ingest_image(&volume, 1, 512)?;
    outcome?;
    let state = scanner.ingest.as_ref().ok_or("state")?;
    assert!(state.slots.base(6).is_none());
    assert_eq!(state.stats.corrupt_records, 1);
    assert_eq!(state.stats.corrupt_headers, 1);
    assert_eq!(
        state.stats.base_records, 3,
        "classification precedes identity rejection"
    );
    Ok(())
}

#[test]
fn ingest_parser_error_categories_and_raw_utf16() -> TestResult {
    let mut invalid_header = named_record(6, vec![65]).build();
    invalid_header
        .get_mut(24..28)
        .ok_or("used size")?
        .copy_from_slice(&1u32.to_le_bytes());
    let mut invalid_fixup = named_record(7, vec![66]).build();
    invalid_fixup
        .get_mut(6..8)
        .ok_or("USA count")?
        .copy_from_slice(&0u16.to_le_bytes());
    let mut invalid_signature = named_record(8, vec![67]).build();
    invalid_signature
        .get_mut(..4)
        .ok_or("signature")?
        .copy_from_slice(b"NOPE");
    let mismatched_type = RecordBuilder {
        record_number: 9,
        attributes: vec![AttrBuilder::resident(0xa0, vec![]).build()],
        ..RecordBuilder::default()
    };
    let bad_value = RecordBuilder {
        record_number: 10,
        attributes: vec![AttrBuilder::resident(0x10, vec![]).build()],
        ..RecordBuilder::default()
    };
    let bad_list = RecordBuilder {
        record_number: 11,
        attributes: vec![AttrBuilder::resident(0x20, vec![1]).build()],
        ..RecordBuilder::default()
    };
    let out_of_volume = RecordBuilder {
        record_number: 12,
        attributes: vec![
            AttrBuilder::non_resident(
                0x80,
                NonResidentModel {
                    mapping_pairs: RunlistEncoder {
                        runs: vec![RunModel {
                            lcn: Some(2048),
                            len: 1,
                        }],
                    }
                    .encode(0),
                    ..NonResidentModel::default()
                },
            )
            .build(),
        ],
        ..RecordBuilder::default()
    };
    let units = vec![0xd800, 65, 0xdc00, 0xd83d, 0xde00];
    let volume = ingest_volume(
        16,
        vec![
            (6, invalid_header),
            (7, invalid_fixup),
            (8, invalid_signature),
            (9, mismatched_type.build()),
            (10, bad_value.build()),
            (11, bad_list.build()),
            (12, out_of_volume.build()),
            (13, named_record(13, units.clone()).build()),
        ],
    )?;
    let (scanner, _, outcome) = ingest_image(&volume, 3, 512)?;
    outcome?;
    let state = scanner.ingest.as_ref().ok_or("state")?;
    assert_eq!(state.stats.corrupt_headers, 1);
    assert_eq!(state.stats.invalid_fixup_metadata, 1);
    assert_eq!(state.stats.invalid_signature, 1);
    assert_eq!(state.stats.type_storage_mismatches, 1);
    assert_eq!(state.stats.corrupt_values, 1);
    assert_eq!(state.stats.invalid_attribute_lists, 1);
    assert_eq!(state.stats.out_of_volume_extents, 1);
    assert_eq!(state.stats.corrupt_records, 7);
    assert_eq!(
        state.stats.invalid_utf16, 0,
        "name conversion is deferred to linking"
    );
    assert_eq!(
        state
            .slots
            .base(13)
            .ok_or("raw name slot")?
            .names
            .first()
            .ok_or("raw name")?
            .name,
        units
    );
    Ok(())
}

#[test]
fn ingest_keeps_stream_segment_identity_sizes_and_typed_metadata() -> TestResult {
    let mut si = vec![0; 48];
    si.get_mut(8..16)
        .ok_or("modified time")?
        .copy_from_slice(&123u64.to_le_bytes());
    si.get_mut(32..36)
        .ok_or("SI attrs")?
        .copy_from_slice(&0x20u32.to_le_bytes());
    let mut standard = AttrBuilder::resident(0x10, si);
    standard.id = 1;
    let mut main = AttrBuilder::resident(0x80, vec![1, 2, 3]);
    main.id = 2;
    let mut ads = AttrBuilder::non_resident(0x80, NonResidentModel::default());
    ads.id = 3;
    ads.name = vec![65];
    let mut continuation = AttrBuilder::non_resident(
        0x80,
        NonResidentModel {
            lowest: 1,
            highest: 1,
            sizes: [-1; 4],
            ..NonResidentModel::default()
        },
    );
    continuation.id = 4;
    continuation.name = vec![65];
    let mut bitmap = AttrBuilder::non_resident(0xb0, NonResidentModel::default());
    bitmap.id = 5;
    let mut reparse = AttrBuilder::resident(0xc0, vec![0x0c, 0, 0, 0xa0, 0, 0, 0, 0]);
    reparse.id = 6;
    let record = RecordBuilder {
        record_number: 6,
        attributes: vec![
            standard.build(),
            main.build(),
            ads.build(),
            continuation.build(),
            bitmap.build(),
            reparse.build(),
        ],
        ..RecordBuilder::default()
    };
    let volume = ingest_volume(16, vec![(6, record.build())])?;
    let (scanner, _, outcome) = ingest_image(&volume, 2, 512)?;
    outcome?;
    let state = scanner.ingest.as_ref().ok_or("state")?;
    let contribution = state.slots.base(6).ok_or("typed contribution")?;
    let standard = contribution
        .standard_information
        .as_ref()
        .ok_or("standard information")?;
    assert!(contribution.standard_information.is_some());
    assert_eq!(standard.value.times().modified(), 123);
    assert_eq!(standard.value.file_attributes().raw(), 0x20);
    assert_eq!(contribution.streams.len(), 4);
    let mut streams = contribution.streams.iter();
    let main = streams.next().ok_or("main stream")?;
    assert_eq!(main.storage, StreamStorage::Resident { size: 3 });
    let ads = streams.next().ok_or("ADS first segment")?;
    assert_eq!(ads.attribute_id, 3);
    assert_eq!(ads.name, vec![65]);
    let StreamStorage::NonResident {
        lowest_vcn,
        sizes,
        has_runs,
        ..
    } = ads.storage
    else {
        return Err("ADS storage".into());
    };
    assert_eq!((lowest_vcn, has_runs), (0, true));
    assert_eq!(sizes.ok_or("ADS sizes")?.data(), 4096);
    let StreamStorage::NonResident {
        lowest_vcn, sizes, ..
    } = streams.next().ok_or("ADS continuation")?.storage
    else {
        return Err("continuation storage".into());
    };
    assert_eq!(lowest_vcn, 1);
    assert!(
        sizes.is_none(),
        "continuation header sizes must remain absent"
    );
    assert_eq!(
        streams.next().ok_or("bitmap stream")?.attribute_type,
        AttributeType::BITMAP
    );
    assert_eq!(
        contribution.reparse_tags.first().ok_or("reparse tag")?.tag,
        0xa000000c
    );
    Ok(())
}

#[test]
fn ingest_small_chunks_reordering_and_transport_bound_are_independent() -> TestResult {
    let volume = ingest_volume(
        16,
        vec![(6, named_record(6, vec![65, 0x4e2d, 0xd83d, 0xde00]).build())],
    )?;
    let (reference, _, outcome) = ingest_image(&volume, 1, 512)?;
    outcome?;
    let expected = reference.ingest.as_ref().ok_or("reference")?;
    for (cap, chunk) in [(3, 512), (4, 4096), (2, 16_384)] {
        let mut scanner = Scanner::new(ScanConfig {
            max_outstanding: cap,
            chunk_size: chunk,
            ..ScanConfig::default()
        })?;
        let (requests, outcome, peak_transport_bytes) =
            drive_to_observed(&mut scanner, &volume, ScanPhase::Merge)?;
        outcome?;
        let mut expected_ranges = vec![(0, 512), (4096, chunk.min(1024))];
        if chunk == 512 {
            expected_ranges.push((4608, 512));
        }
        for offset in (4096u64..20_480).step_by(chunk) {
            expected_ranges.push((
                offset,
                usize::try_from(20_480u64.checked_sub(offset).ok_or("read end")?)?.min(chunk),
            ));
        }
        assert_eq!(
            requests
                .iter()
                .map(|request| (request.offset, request.len))
                .collect::<Vec<_>>(),
            expected_ranges,
            "ingest request ranges at cap {cap}, chunk {chunk}"
        );
        for (index, request) in requests.iter().enumerate() {
            assert_eq!(request.id.raw(), u64::try_from(index)?);
        }
        let actual = scanner.ingest.as_ref().ok_or("state")?;
        assert_eq!(actual.slots, expected.slots);
        assert_eq!(actual.patches, expected.patches);
        assert_eq!(actual.stats, expected.stats);
        let bound = cap
            .checked_mul(chunk)
            .and_then(|n| n.checked_add(1024))
            .ok_or("expected transport bound")?;
        assert!(peak_transport_bytes <= bound);
        if (cap, chunk) == (4, 4096) {
            assert_eq!(
                peak_transport_bytes, bound,
                "test must fill all response slots"
            );
        }
    }
    Ok(())
}

#[test]
fn ingest_issues_before_consuming_and_retains_partial_response_slots() -> TestResult {
    let volume = ingest_volume(700, vec![])?;
    let mut scanner = Scanner::new(ScanConfig {
        max_outstanding: 3,
        chunk_size: 196_608,
        ..ScanConfig::default()
    })?;
    drive_to(&mut scanner, &volume, ScanPhase::Stream)?.1?;
    assert!(matches!(scanner.step()?, ScanStep::Progress(_)));
    for _ in 0..3 {
        let ScanStep::Need(request) = scanner.step()? else {
            return Err("stream must fill available request slots before consuming".into());
        };
        let start = usize::try_from(request.offset)?;
        let end = start.checked_add(request.len).ok_or("response end")?;
        scanner.feed(request.id, volume.get(start..end).ok_or("response bytes")?)?;
        assert_eq!(
            scanner.ingest.as_ref().ok_or("ingest")?.stats.records_total,
            0
        );
    }
    let ScanStep::Progress(progress) = scanner.step()? else {
        return Err("full request queue must consume a bounded record batch".into());
    };
    assert_eq!(progress.records_processed, 256);
    assert!(!progress.waiting_for_input);
    assert_eq!(scanner.requests.len(), 2);
    assert_eq!(
        scanner
            .requests
            .ready_bytes()
            .ok_or("partial response")?
            .len(),
        131_072
    );

    let ScanStep::Need(request) = scanner.step()? else {
        return Err("freed request capacity must be filled before further consumption".into());
    };
    assert_eq!((request.offset, request.len), (593_920, 126_976));
    assert_eq!(
        scanner.ingest.as_ref().ok_or("ingest")?.stats.records_total,
        256
    );
    assert_eq!(scanner.requests.len(), 3);
    assert_eq!(
        scanner
            .requests
            .ready_bytes()
            .ok_or("retained response")?
            .len(),
        131_072
    );
    let start = usize::try_from(request.offset)?;
    let end = start.checked_add(request.len).ok_or("response end")?;
    scanner.feed(request.id, volume.get(start..end).ok_or("response bytes")?)?;
    drive_to(&mut scanner, &volume, ScanPhase::Merge)?.1?;
    assert_eq!(
        scanner.ingest.as_ref().ok_or("ingest")?.stats.records_total,
        700
    );
    assert_eq!(scanner.requests.len(), 0);
    Ok(())
}

#[test]
fn ingest_cpu_steps_yield_within_256_records() -> TestResult {
    let volume = ingest_volume(700, vec![])?;
    for (cap, chunk) in [(1, 1_048_576), (4, 131_072), (3, 262_144)] {
        let (scanner, _, outcome) = ingest_image(&volume, cap, chunk)?;
        outcome?;
        let state = scanner.ingest.as_ref().ok_or("state")?;
        assert_eq!(state.stats.records_total, 700);
        assert_eq!(state.slots.len(), 700);
    }
    Ok(())
}

#[test]
fn ingest_corrupt_or_missing_root_is_fatal() -> TestResult {
    let volume = ingest_volume(16, vec![(5, vec![0; 1024])])?;
    let (mut scanner, _, outcome) = ingest_image(&volume, 3, 512)?;
    assert!(matches!(
        outcome,
        Err(ScanError::RequiredRecord { record: 5 })
    ));
    assert!(matches!(scanner.step(), Err(ScanError::Inactive)));
    let volume = ingest_volume(4, vec![])?;
    assert!(matches!(
        ingest_image(&volume, 2, 1024)?.2,
        Err(ScanError::RequiredRecord { record: 5 })
    ));
    Ok(())
}

fn ingest_fixture(name: &str) -> TestResult {
    let fixture = Fixture::load(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/ntfs")
            .join(format!("{name}.json")),
    )?;
    let (scanner, _, outcome) = ingest_image(fixture.image(), 4, 512)?;
    outcome?;
    let state = scanner.ingest.as_ref().ok_or("fixture ingestion")?;
    let expected_total = fixture
        .manifest()
        .mft
        .data_size
        .checked_div(u64::from(fixture.manifest().geometry.file_record_size))
        .ok_or("fixture record count")?;
    assert_eq!(state.stats.records_total, expected_total);
    assert_eq!(u64::try_from(state.slots.len())?, expected_total);
    for entry in fixture
        .manifest()
        .entries
        .iter()
        .chain(&fixture.manifest().metafiles)
    {
        let slot = state
            .slots
            .get(usize::try_from(entry.record)?)
            .ok_or("manifest slot")?;
        assert_eq!(
            slot.classification,
            RecordKind::Base,
            "{name}: {}",
            entry.path
        );
        let base = state
            .slots
            .base(usize::try_from(entry.record)?)
            .ok_or_else(|| {
                format!(
                    "{name}: {} has no base contribution; stats {:?}",
                    entry.path, state.stats
                )
            })?;
        assert_eq!(
            base.reference.sequence(),
            entry.seq,
            "{name}: {}",
            entry.path
        );
        for expected in &entry.names {
            assert!(
                base.names
                    .iter()
                    .chain(
                        state
                            .patches
                            .iter()
                            .filter(|patch| patch.base == base.reference)
                            .flat_map(|patch| &patch.contribution.names)
                    )
                    .any(|actual| actual.parent.record() == expected.parent_record
                        && actual.parent.sequence() == expected.parent_seq
                        && actual.namespace == expected.namespace
                        && actual.name == expected.name),
                "{name}: missing name for record {}: {}",
                entry.record,
                entry.path
            );
        }
    }
    if name == "basic-4k" {
        assert_eq!(
            state
                .slots
                .get(322)
                .ok_or("deleted record 322")?
                .classification,
            RecordKind::Deleted
        );
    }
    if name == "frag-512" {
        let slot = state.slots.get(4095).ok_or("cross-extent record 4095")?;
        assert_eq!(slot.classification, RecordKind::Deleted);
        assert!(state.slots.base(4095).is_none());
        let boundary = 8191u64.checked_mul(512).ok_or("extent boundary")?;
        assert_eq!(
            boundary.checked_sub(4095u64.checked_mul(1024).ok_or("record offset")?),
            Some(512)
        );
    }
    Ok(())
}

#[test]
fn ingest_fixture_basic_4k_and_deleted_record_322() -> TestResult {
    ingest_fixture("basic-4k")
}

#[test]
fn ingest_fixture_frag_512_record_4095_reassembles_across_extents() -> TestResult {
    ingest_fixture("frag-512")
}

#[test]
fn ingest_fixture_native_4kn_64k() -> TestResult {
    ingest_fixture("4kn-64k")
}

#[test]
fn ingest_frag_record_4095_restores_fixup_and_name_across_extents() -> TestResult {
    let fixture = Fixture::load(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/ntfs/frag-512.json"),
    )?;
    let mut volume = fixture.image().to_vec();
    let mut name = vec![65; 220];
    name.push(0x4e2d);
    let raw = named_record(4095, name.clone()).build();
    // Oracle offsets locate the two halves; Scanner still bootstraps from volume bytes.
    let first = fixture
        .manifest()
        .mft
        .extents
        .first()
        .ok_or("first MFT extent")?;
    let next = fixture
        .manifest()
        .mft
        .extents
        .get(1)
        .ok_or("second MFT extent")?;
    let first_lcn = first
        .lcn
        .ok_or("physical first MFT run")?
        .checked_add(8190)
        .ok_or("record first LCN")?;
    let first_offset = usize::try_from(
        first_lcn
            .checked_mul(512)
            .ok_or("record first physical offset")?,
    )?;
    let second_offset = usize::try_from(
        next.lcn
            .ok_or("physical second MFT run")?
            .checked_mul(512)
            .ok_or("record second physical offset")?,
    )?;
    put(
        &mut volume,
        first_offset,
        raw.get(..512).ok_or("record first half")?,
    )?;
    put(
        &mut volume,
        second_offset,
        raw.get(512..).ok_or("record second half")?,
    )?;
    for (cap, chunk) in [(1, 512), (4, 4096)] {
        let (scanner, _, outcome) = ingest_image(&volume, cap, chunk)?;
        outcome?;
        let state = scanner.ingest.as_ref().ok_or("state")?;
        let slot = state.slots.get(4095).ok_or("cross-extent slot")?;
        assert_eq!(slot.classification, RecordKind::Base);
        assert_eq!(
            state
                .slots
                .base(4095)
                .ok_or("cross-extent contribution")?
                .names
                .first()
                .ok_or("cross-extent name")?
                .name,
            name
        );
    }
    Ok(())
}

#[test]
fn ingest_unallocated_zero_record_number_is_not_corruption() -> TestResult {
    let unallocated = RecordBuilder {
        flags: 0,
        record_number: 0,
        ..RecordBuilder::default()
    };
    let mismatched_deleted = RecordBuilder {
        flags: 0,
        record_number: 99,
        ..RecordBuilder::default()
    };
    let mismatched_live = RecordBuilder {
        flags: 1,
        record_number: 0,
        ..RecordBuilder::default()
    };
    let volume = ingest_volume(
        16,
        vec![
            (6, unallocated.build()),
            (7, mismatched_deleted.build()),
            (8, mismatched_live.build()),
        ],
    )?;
    let (scanner, _, outcome) = ingest_image(&volume, 3, 512)?;
    outcome?;
    let state = scanner.ingest.as_ref().ok_or("ingestion state")?;
    assert_eq!(
        state.slots.get(6).ok_or("unallocated slot")?.classification,
        RecordKind::Deleted
    );
    assert!(state.slots.base(6).is_none());
    assert!(state.slots.base(8).is_none());
    assert_eq!(state.stats.deleted, 2);
    assert_eq!(
        state.stats.corrupt_records, 2,
        "only nonzero deleted mismatches and live mismatches are corrupt"
    );
    assert_eq!(state.stats.corrupt_headers, 2);
    Ok(())
}

#[test]
fn ingest_contribution_arrays_match_decoded_attribute_counts() -> TestResult {
    for count in [0, 1, 5] {
        let mut attributes = vec![
            AttrBuilder::resident(0x10, vec![0; 48]).build(),
            AttrBuilder::resident(0x10, vec![0; 48]).build(),
        ];
        for id in 0..count {
            let mut stream = AttrBuilder::resident(0x80, vec![1]);
            stream.id = id;
            attributes.push(stream.build());
            let mut tag = AttrBuilder::resident(0xc0, vec![0x0c, 0, 0, 0xa0, 0, 0, 0, 0]);
            tag.id = id + 10;
            attributes.push(tag.build());
        }
        for id in 0..count.min(2) {
            let mut name = AttrBuilder::resident(0x30, FileNameBuilder::default().build());
            name.id = id + 20;
            attributes.push(name.build());
        }
        let record = RecordBuilder {
            record_number: 6,
            attributes,
            ..RecordBuilder::default()
        };
        let volume = ingest_volume(16, vec![(6, record.build())])?;
        let (scanner, _, outcome) = ingest_image(&volume, 2, 512)?;
        outcome?;
        let base = scanner
            .ingest
            .as_ref()
            .ok_or("ingest")?
            .slots
            .base(6)
            .ok_or("base")?;
        assert_eq!(base.streams.len(), usize::from(count));
        assert_eq!(base.streams.capacity(), base.streams.len());
        assert_eq!(base.reparse_tags.len(), usize::from(count));
        assert_eq!(base.reparse_tags.capacity(), base.reparse_tags.len());
        assert_eq!(base.names.len(), usize::from(count.min(2)));
        assert_eq!(base.names.capacity(), base.names.len());
        assert!(base.standard_information.is_some());
        assert!(!base.conflicting_standard_information);
    }
    Ok(())
}

#[test]
fn ingest_attribute_counting_keeps_first_value_error_diagnostic() -> TestResult {
    let invalid_value = AttrBuilder::resident(0x10, Vec::new()).build();
    let mut invalid_attribute = AttrBuilder::resident(0x80, Vec::new()).build();
    invalid_attribute
        .get_mut(..4)
        .ok_or("attribute type")?
        .fill(0);
    let record = RecordBuilder {
        record_number: 6,
        attributes: vec![invalid_value, invalid_attribute],
        ..RecordBuilder::default()
    };
    let volume = ingest_volume(16, vec![(6, record.build())])?;
    let (scanner, _, outcome) = ingest_image(&volume, 2, 512)?;
    outcome?;
    let state = scanner.ingest.as_ref().ok_or("ingest")?;
    assert!(state.slots.base(6).is_none());
    assert_eq!(state.stats.corrupt_values, 1);
    assert_eq!(state.stats.corrupt_attributes, 0);
    Ok(())
}
