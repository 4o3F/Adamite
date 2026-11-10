//! Discovery phase regressions.

use super::support::{DriverOutcome, TestResult, boot, drive_to, image, put, record, segment};
use crate::error::ScanError;
use crate::mft::mapping::OwnedRun;
use crate::scan::{ScanConfig, ScanPhase, Scanner};
use adamite_fixtures::Fixture;
use adamite_fixtures::builders::{
    AttrBody, AttrBuilder, AttrListBuilder, AttrListEntryModel, BootSectorBuilder,
    NonResidentModel, RecordBuilder,
};
use adamite_fixtures::runlist_encoder::{RunModel, RunlistEncoder};
use adamite_ntfs::record::RecordError;
use std::error::Error;
use std::path::Path;

fn reference(record: u64, id: u16, lowest: i64) -> AttrListEntryModel {
    AttrListEntryModel {
        type_code: 0x80,
        lowest,
        segment: (7u64 << 48) | record,
        id,
        name: Vec::new(),
    }
}

fn resident_list(entries: Vec<AttrListEntryModel>) -> AttrBuilder {
    let mut attr = AttrBuilder::resident(
        0x20,
        AttrListBuilder {
            entries,
            zero_tail: 0,
        }
        .build(),
    );
    attr.id = 10;
    attr
}

fn extension(number: u32, attributes: Vec<Vec<u8>>) -> RecordBuilder {
    RecordBuilder {
        record_number: number,
        base: 7u64 << 48,
        attributes,
        ..RecordBuilder::default()
    }
}

fn discovery_image(
    base_attrs: Vec<Vec<u8>>,
    first: &RecordBuilder,
    second: &RecordBuilder,
) -> Result<Vec<u8>, Box<dyn Error>> {
    let boot = BootSectorBuilder {
        cluster_encoding: 1,
        mft: 8,
        mirror: 64,
        ..boot()
    };
    let base = RecordBuilder {
        attributes: base_attrs,
        ..record()
    }
    .build();
    let mut image = image(&boot, &base)?;
    put(&mut image, 5120, &first.build())?;
    put(&mut image, 17_408, &second.build())?;
    Ok(image)
}

fn chain_image() -> Result<Vec<u8>, Box<dyn Error>> {
    discovery_image(
        vec![
            segment(0, 0, 3, 8, 4).build(),
            resident_list(vec![
                reference(0, 0, 0),
                reference(3, 2, 8),
                reference(1, 1, 4),
            ])
            .build(),
        ],
        &extension(1, vec![segment(1, 4, 7, 32, 4).build()]),
        &extension(3, vec![segment(2, 8, 11, 48, 4).build()]),
    )
}

fn discover_image(image: &[u8], cap: usize, chunk: usize) -> Result<DriverOutcome, Box<dyn Error>> {
    let mut scanner = Scanner::new(ScanConfig {
        max_outstanding: cap,
        chunk_size: chunk,
        ..ScanConfig::default()
    })?;
    let (requests, outcome) = drive_to(&mut scanner, image, ScanPhase::Stream)?;
    Ok((scanner, requests, outcome))
}

fn chain_runs() -> Vec<OwnedRun> {
    vec![
        OwnedRun {
            vcn: 0,
            lcn: 8,
            clusters: 4,
        },
        OwnedRun {
            vcn: 4,
            lcn: 32,
            clusters: 4,
        },
        OwnedRun {
            vcn: 8,
            lcn: 48,
            clusters: 4,
        },
    ]
}

#[test]
fn bootstrap_multipass_discovery_caps_chunks_and_response_orders() -> TestResult {
    let image = chain_image()?;
    for cap in [1, 2, 4] {
        for chunk in [512, 1024, 4096] {
            let (scanner, requests, outcome) = discover_image(&image, cap, chunk)?;
            outcome?;
            let base = scanner.bootstrap.as_ref().ok_or("complete MFT mapping")?;
            assert_eq!(base.mapping.runs, chain_runs());
            assert_eq!(base.data_size, 6144);
            let expected = if chunk == 512 {
                vec![
                    (0, 0, 512),
                    (1, 4096, 512),
                    (2, 4608, 512),
                    (3, 5120, 512),
                    (4, 5632, 512),
                    (5, 17_408, 512),
                    (6, 17_920, 512),
                ]
            } else {
                vec![
                    (0, 0, 512),
                    (1, 4096, 1024),
                    (2, 5120, 1024),
                    (3, 17_408, 1024),
                ]
            };
            assert_eq!(
                requests
                    .iter()
                    .map(|request| (request.id.raw(), request.offset, request.len))
                    .collect::<Vec<_>>(),
                expected,
                "bootstrap request identities and order at cap {cap}, chunk {chunk}"
            );
            let first = requests
                .iter()
                .position(|request| request.offset == 5120)
                .ok_or("first extension read")?;
            let second = requests
                .iter()
                .position(|request| request.offset == 17_408)
                .ok_or("second extension read")?;
            assert!(
                first < second,
                "unreachable extension must wait for the earlier extension's runs"
            );
            assert_eq!(scanner.phase, ScanPhase::Stream);
        }
    }
    Ok(())
}

#[test]
fn bootstrap_rejects_stale_sequence_base_and_record_identity() -> TestResult {
    for first in [
        RecordBuilder {
            sequence: 8,
            ..extension(1, vec![segment(1, 4, 7, 32, 4).build()])
        },
        RecordBuilder {
            base: 6u64 << 48,
            ..extension(1, vec![segment(1, 4, 7, 32, 4).build()])
        },
        RecordBuilder {
            base: (7u64 << 48) | 2,
            ..extension(1, vec![segment(1, 4, 7, 32, 4).build()])
        },
        RecordBuilder {
            flags: 0,
            ..extension(1, vec![segment(1, 4, 7, 32, 4).build()])
        },
        extension(2, vec![segment(1, 4, 7, 32, 4).build()]),
    ] {
        let image = discovery_image(
            vec![
                segment(0, 0, 3, 8, 4).build(),
                resident_list(vec![reference(1, 1, 4)]).build(),
            ],
            &first,
            &extension(3, vec![]),
        )?;
        let (mut scanner, _, outcome) = discover_image(&image, 3, 512)?;
        assert!(
            matches!(outcome, Err(ScanError::Bootstrap { .. })),
            "{outcome:?}"
        );
        assert_eq!(
            scanner.bootstrap.as_ref().ok_or("base")?.mapping.runs.len(),
            1,
            "invalid extension committed runs"
        );
        assert!(matches!(scanner.step(), Err(ScanError::Inactive)));
    }
    Ok(())
}

#[test]
fn bootstrap_missing_target_and_segment_identity_are_fatal() -> TestResult {
    for target in [reference(1, 99, 4), reference(1, 1, 5), reference(6, 1, 4)] {
        let image = discovery_image(
            vec![
                segment(0, 0, 3, 8, 4).build(),
                resident_list(vec![target]).build(),
            ],
            &extension(1, vec![segment(1, 4, 7, 32, 4).build()]),
            &extension(3, vec![]),
        )?;
        let (_, _, outcome) = discover_image(&image, 1, 512)?;
        assert!(matches!(outcome, Err(ScanError::Bootstrap { .. })));
    }
    let mut image = chain_image()?;
    image
        .get_mut(5120..6144)
        .ok_or("missing extension record")?
        .fill(0);
    let (_, _, outcome) = discover_image(&image, 2, 512)?;
    assert!(
        matches!(
            outcome,
            Err(ScanError::Record {
                source: RecordError::InvalidSignature {
                    signature: [0, 0, 0, 0]
                }
            })
        ),
        "{outcome:?}"
    );
    Ok(())
}

#[test]
fn bootstrap_rejects_conflicting_references_and_overlapping_mappings() -> TestResult {
    let mut stale = reference(1, 1, 4);
    stale.segment = (8u64 << 48) | 1;
    let image = discovery_image(
        vec![
            segment(0, 0, 3, 8, 4).build(),
            resident_list(vec![reference(1, 1, 4), stale]).build(),
        ],
        &extension(1, vec![segment(1, 4, 7, 32, 4).build()]),
        &extension(3, vec![]),
    )?;
    assert!(matches!(
        discover_image(&image, 3, 512)?.2,
        Err(ScanError::Bootstrap { .. })
    ));
    for (lowest, highest, lcn, len) in [(3, 7, 32, 5), (4, 7, 9, 4)] {
        let image = discovery_image(
            vec![
                segment(0, 0, 3, 8, 4).build(),
                resident_list(vec![reference(1, 1, lowest)]).build(),
            ],
            &extension(1, vec![segment(1, lowest, highest, lcn, len).build()]),
            &extension(3, vec![]),
        )?;
        let (scanner, _, outcome) = discover_image(&image, 2, 512)?;
        assert!(matches!(outcome, Err(ScanError::Bootstrap { .. })));
        assert_eq!(
            scanner.bootstrap.as_ref().ok_or("base")?.mapping.runs.len(),
            1
        );
    }
    Ok(())
}

#[test]
fn bootstrap_gap_and_partial_extension_reachability_terminate() -> TestResult {
    let mut image = discovery_image(
        vec![
            segment(0, 0, 3, 8, 4).build(),
            resident_list(vec![reference(1, 1, 5), reference(3, 2, 8)]).build(),
        ],
        &extension(1, vec![segment(1, 5, 7, 32, 3).build()]),
        &extension(3, vec![segment(2, 8, 11, 48, 4).build()]),
    )?;
    put(
        &mut image,
        16_896,
        &extension(3, vec![segment(2, 8, 11, 48, 4).build()]).build(),
    )?;
    assert!(matches!(
        discover_image(&image, 3, 512)?.2,
        Err(ScanError::BootstrapIncomplete)
    ));
    let image = discovery_image(
        vec![
            segment(0, 0, 2, 8, 3).build(),
            resident_list(vec![reference(1, 1, 3)]).build(),
        ],
        &extension(1, vec![segment(1, 3, 11, 32, 9).build()]),
        &extension(3, vec![]),
    )?;
    let (_, requests, outcome) = discover_image(&image, 3, 512)?;
    assert!(matches!(outcome, Err(ScanError::BootstrapIncomplete)));
    assert!(
        requests.iter().all(|request| request.offset < 5120),
        "partially reachable extension must not be requested"
    );
    Ok(())
}

#[test]
fn bootstrap_duplicate_discoveries_reuse_validated_extension() -> TestResult {
    let mut later = resident_list(vec![reference(1, 1, 4), reference(3, 2, 8)]);
    later.name = vec![65];
    let image = discovery_image(
        vec![
            segment(0, 0, 3, 8, 4).build(),
            resident_list(vec![reference(1, 1, 4), reference(1, 1, 4)]).build(),
        ],
        &extension(1, vec![segment(1, 4, 7, 32, 4).build(), later.build()]),
        &extension(3, vec![segment(2, 8, 11, 48, 4).build()]),
    )?;
    let (scanner, requests, outcome) = discover_image(&image, 4, 1024)?;
    outcome?;
    assert_eq!(
        scanner.bootstrap.as_ref().ok_or("base")?.mapping.runs,
        chain_runs()
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.offset == 5120)
            .count(),
        1
    );
    Ok(())
}

fn nonresident_list(size: i64, lowest: i64, highest: i64, runs: Vec<RunModel>) -> AttrBuilder {
    let mut attr = AttrBuilder::non_resident(
        0x20,
        NonResidentModel {
            lowest,
            highest,
            sizes: [size, size, size, 0],
            mapping_pairs: RunlistEncoder { runs }.encode(0),
            ..NonResidentModel::default()
        },
    );
    attr.id = 10;
    attr
}

fn list_value() -> Vec<u8> {
    let mut entries = vec![reference(0, 0, 0); 30];
    entries.push(reference(1, 1, 4));
    entries.push(reference(3, 2, 8));
    AttrListBuilder {
        entries,
        zero_tail: 0,
    }
    .build()
}

#[test]
fn bootstrap_nonresident_list_uses_own_extents_and_small_chunks() -> TestResult {
    let value = list_value();
    assert_eq!(value.len(), 1024);
    let list = nonresident_list(
        1024,
        0,
        1,
        vec![
            RunModel {
                lcn: Some(80),
                len: 1,
            },
            RunModel {
                lcn: Some(90),
                len: 1,
            },
        ],
    );
    let mut image = discovery_image(
        vec![segment(0, 0, 3, 8, 4).build(), list.build()],
        &extension(1, vec![segment(1, 4, 7, 32, 4).build()]),
        &extension(3, vec![segment(2, 8, 11, 48, 4).build()]),
    )?;
    put(
        &mut image,
        40_960,
        value.get(..512).ok_or("list first extent")?,
    )?;
    put(
        &mut image,
        46_080,
        value.get(512..).ok_or("list second extent")?,
    )?;
    for cap in [1, 3] {
        let (scanner, requests, outcome) = discover_image(&image, cap, 512)?;
        outcome?;
        assert_eq!(
            scanner.bootstrap.as_ref().ok_or("base")?.mapping.runs,
            chain_runs()
        );
        assert!(
            requests
                .iter()
                .any(|request| request.offset == 40_960 && request.len == 512)
        );
        assert!(
            requests
                .iter()
                .any(|request| request.offset == 46_080 && request.len == 512)
        );
    }
    Ok(())
}

#[test]
fn bootstrap_additional_list_descriptors_make_incomplete_list_readable() -> TestResult {
    let value = list_value();
    let first = nonresident_list(
        1024,
        0,
        0,
        vec![RunModel {
            lcn: Some(80),
            len: 1,
        }],
    );
    let mut continuation = nonresident_list(
        0,
        1,
        1,
        vec![RunModel {
            lcn: Some(90),
            len: 1,
        }],
    );
    continuation.id = 11;
    let mut reachable = resident_list(vec![reference(1, 1, 4)]);
    reachable.name = vec![65];
    reachable.id = 12;
    let mut image = discovery_image(
        vec![
            segment(0, 0, 3, 8, 4).build(),
            first.build(),
            reachable.build(),
        ],
        &extension(
            1,
            vec![segment(1, 4, 7, 32, 4).build(), continuation.build()],
        ),
        &extension(3, vec![segment(2, 8, 11, 48, 4).build()]),
    )?;
    put(
        &mut image,
        40_960,
        value.get(..512).ok_or("list first half")?,
    )?;
    put(
        &mut image,
        46_080,
        value.get(512..).ok_or("list second half")?,
    )?;
    let (scanner, requests, outcome) = discover_image(&image, 3, 512)?;
    outcome?;
    assert_eq!(
        scanner.bootstrap.as_ref().ok_or("base")?.mapping.runs,
        chain_runs()
    );
    let extension = requests
        .iter()
        .position(|request| request.offset == 5120)
        .ok_or("extension request")?;
    let list = requests
        .iter()
        .position(|request| request.offset == 40_960)
        .ok_or("list request")?;
    assert!(
        extension < list,
        "complete list must wait for its continuation descriptor"
    );
    Ok(())
}

#[test]
fn bootstrap_inaccessible_or_oversized_list_terminates_before_io() -> TestResult {
    for (size, expected_oversized) in [(1024, false), (262_145, true)] {
        let list = nonresident_list(
            size,
            0,
            0,
            vec![RunModel {
                lcn: Some(80),
                len: 1,
            }],
        );
        let image = discovery_image(
            vec![segment(0, 0, 3, 8, 4).build(), list.build()],
            &extension(1, vec![]),
            &extension(3, vec![]),
        )?;
        let (_, requests, outcome) = discover_image(&image, 3, 512)?;
        if expected_oversized {
            assert!(matches!(
                outcome,
                Err(ScanError::AttributeListTooLarge { .. })
            ));
        } else {
            assert!(matches!(outcome, Err(ScanError::BootstrapIncomplete)));
        }
        assert!(
            requests.iter().all(|request| request.offset < 5120),
            "inaccessible list caused a speculative read"
        );
    }
    Ok(())
}

#[test]
fn bootstrap_rejects_sparse_compressed_mft_and_list_storage() -> TestResult {
    for flags in [1, 0x8000] {
        let mut bad_segment = segment(1, 4, 7, 32, 4);
        bad_segment.flags = flags;
        let image = discovery_image(
            vec![
                segment(0, 0, 3, 8, 4).build(),
                resident_list(vec![reference(1, 1, 4)]).build(),
            ],
            &extension(1, vec![bad_segment.build()]),
            &extension(3, vec![]),
        )?;
        assert!(matches!(
            discover_image(&image, 3, 512)?.2,
            Err(ScanError::Bootstrap { .. })
        ));
        let mut list = nonresident_list(
            32,
            0,
            0,
            vec![RunModel {
                lcn: Some(80),
                len: 1,
            }],
        );
        list.flags = flags;
        let image = discovery_image(
            vec![segment(0, 0, 3, 8, 4).build(), list.build()],
            &extension(1, vec![]),
            &extension(3, vec![]),
        )?;
        assert!(matches!(
            discover_image(&image, 3, 512)?.2,
            Err(ScanError::Bootstrap { .. })
        ));
    }
    let list = nonresident_list(32, 0, 0, vec![RunModel { lcn: None, len: 1 }]);
    let image = discovery_image(
        vec![segment(0, 0, 3, 8, 4).build(), list.build()],
        &extension(1, vec![]),
        &extension(3, vec![]),
    )?;
    assert!(matches!(
        discover_image(&image, 3, 512)?.2,
        Err(ScanError::Bootstrap { .. })
    ));
    Ok(())
}

#[test]
fn bootstrap_malformed_list_rejects_valid_prefix_before_reading_extensions() -> TestResult {
    let mut value = resident_list(vec![reference(1, 1, 4)]);
    let AttrBody::Resident(bytes) = &mut value.body else {
        return Err("resident test list".into());
    };
    bytes.push(1);
    let image = discovery_image(
        vec![segment(0, 0, 3, 8, 4).build(), value.build()],
        &extension(1, vec![segment(1, 4, 7, 32, 4).build()]),
        &extension(3, vec![]),
    )?;
    let (_, requests, outcome) = discover_image(&image, 3, 512)?;
    assert!(matches!(outcome, Err(ScanError::AttributeList { .. })));
    assert!(requests.iter().all(|request| request.offset < 5120));
    Ok(())
}

#[test]
fn bootstrap_reachable_extension_assembles_across_extent_boundaries() -> TestResult {
    let mut first = segment(0, 0, 3, 8, 4);
    let AttrBody::NonResident(model) = &mut first.body else {
        return Err("nonresident test segment".into());
    };
    model.mapping_pairs = RunlistEncoder {
        runs: vec![
            RunModel {
                lcn: Some(8),
                len: 3,
            },
            RunModel {
                lcn: Some(60),
                len: 1,
            },
        ],
    }
    .encode(0);
    let ext = extension(1, vec![segment(1, 4, 7, 32, 4).build()]).build();
    let mut image = discovery_image(
        vec![
            first.build(),
            resident_list(vec![reference(1, 1, 4), reference(3, 2, 8)]).build(),
        ],
        &extension(1, vec![]),
        &extension(3, vec![segment(2, 8, 11, 48, 4).build()]),
    )?;
    put(
        &mut image,
        5120,
        ext.get(..512).ok_or("extension first half")?,
    )?;
    put(
        &mut image,
        30_720,
        ext.get(512..).ok_or("extension second half")?,
    )?;
    let (scanner, requests, outcome) = discover_image(&image, 4, 4096)?;
    outcome?;
    assert_eq!(
        scanner.bootstrap.as_ref().ok_or("base")?.mapping.runs.len(),
        4
    );
    assert!(
        requests
            .iter()
            .any(|request| request.offset == 5120 && request.len == 512)
    );
    assert!(
        requests
            .iter()
            .any(|request| request.offset == 30_720 && request.len == 512)
    );
    Ok(())
}

fn complete_fixture(name: &str) -> TestResult {
    let fixture = Fixture::load(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/ntfs")
            .join(format!("{name}.json")),
    )?;
    for (cap, chunk) in [(1, 512), (3, 512), (4, 4096)] {
        let (scanner, _, outcome) = discover_image(fixture.image(), cap, chunk)?;
        outcome?;
        let base = scanner.bootstrap.as_ref().ok_or("complete fixture MFT")?;
        assert_eq!(base.data_size, fixture.manifest().mft.data_size, "{name}");
        let extents = base
            .mapping
            .runs
            .iter()
            .map(|run| adamite_fixtures::Extent {
                vcn: run.vcn,
                lcn: Some(run.lcn),
                len: run.clusters,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            extents,
            fixture.manifest().mft.extents,
            "{name}: complete ordered extents"
        );
        assert_eq!(scanner.phase, ScanPhase::Stream);
    }
    Ok(())
}

#[test]
fn bootstrap_complete_fixture_basic_4k() -> TestResult {
    complete_fixture("basic-4k")
}

#[test]
fn bootstrap_complete_fixture_frag_512() -> TestResult {
    complete_fixture("frag-512")
}

#[test]
fn bootstrap_complete_fixture_4kn_64k() -> TestResult {
    complete_fixture("4kn-64k")
}

#[test]
fn bootstrap_multiple_data_segments_in_one_extension_are_read_once() -> TestResult {
    let image = discovery_image(
        vec![
            segment(0, 0, 3, 8, 4).build(),
            resident_list(vec![reference(1, 1, 4), reference(1, 2, 8)]).build(),
        ],
        &extension(
            1,
            vec![
                segment(1, 4, 7, 32, 4).build(),
                segment(2, 8, 11, 48, 4).build(),
            ],
        ),
        &extension(3, vec![]),
    )?;
    let (scanner, requests, outcome) = discover_image(&image, 4, 1024)?;
    outcome?;
    assert_eq!(
        scanner
            .bootstrap
            .as_ref()
            .ok_or("complete base")?
            .mapping
            .runs,
        chain_runs()
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.offset == 5120)
            .count(),
        1
    );
    Ok(())
}

#[test]
fn bootstrap_attribute_list_references_validate_name_id_and_lowest_vcn() -> TestResult {
    for (id, name, lowest, valid) in [
        (12, vec![65], 0, true),
        (13, vec![65], 0, false),
        (12, vec![66], 0, false),
        (12, vec![65], 1, false),
    ] {
        let mut later = resident_list(vec![reference(3, 2, 8)]);
        later.name = vec![65];
        later.id = 12;
        let list_ref = AttrListEntryModel {
            type_code: 0x20,
            segment: (7u64 << 48) | 1,
            id,
            name,
            lowest,
        };
        let image = discovery_image(
            vec![
                segment(0, 0, 3, 8, 4).build(),
                resident_list(vec![reference(1, 1, 4), list_ref]).build(),
            ],
            &extension(1, vec![segment(1, 4, 7, 32, 4).build(), later.build()]),
            &extension(3, vec![segment(2, 8, 11, 48, 4).build()]),
        )?;
        let (_, _, outcome) = discover_image(&image, 3, 512)?;
        if valid {
            outcome?;
        } else {
            assert!(matches!(outcome, Err(ScanError::Bootstrap { .. })));
        }
    }
    Ok(())
}

#[test]
fn bootstrap_accepts_complete_256k_list_with_bounded_entry_steps() -> TestResult {
    let boot = BootSectorBuilder {
        sectors: 1024,
        cluster_encoding: 1,
        mft: 8,
        mirror: 64,
        ..boot()
    };
    let value = AttrListBuilder {
        entries: vec![reference(0, 0, 0); 8192],
        zero_tail: 0,
    }
    .build();
    assert_eq!(value.len(), 262_144);
    let list = nonresident_list(
        262_144,
        0,
        511,
        vec![RunModel {
            lcn: Some(128),
            len: 512,
        }],
    );
    let base = RecordBuilder {
        attributes: vec![segment(0, 0, 11, 8, 12).build(), list.build()],
        ..record()
    };
    let mut image = image(&boot, &base.build())?;
    image.resize(524_288, 0);
    put(&mut image, 65_536, &value)?;
    let (scanner, requests, outcome) = discover_image(&image, 4, 512)?;
    outcome?;
    assert_eq!(
        scanner
            .bootstrap
            .as_ref()
            .ok_or("complete base")?
            .mapping
            .runs
            .len(),
        1
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.offset >= 65_536)
            .count(),
        512
    );
    Ok(())
}
