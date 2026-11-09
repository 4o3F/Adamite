use super::*;
use std::error::Error;

#[test]
fn protocol_done_transfers_output_once_then_rejects_calls() -> Result<(), Box<dyn Error>> {
    let mut scanner = Scanner::new(ScanConfig::default())?;
    // Finalization is not implemented in M4; seed its terminal handoff directly.
    scanner.phase = ScanPhase::Done;
    scanner.output = Some(ScanOutput {
        snapshot: Snapshot::default(),
        stats: ScanStats::default(),
    });
    assert!(matches!(scanner.step()?, ScanStep::Done(_)));
    for _ in 0..3 {
        assert!(matches!(scanner.step(), Err(ScanError::Inactive)));
        assert!(matches!(
            scanner.feed(RequestId::from_raw(0), &[]),
            Err(FeedError::Inactive)
        ));
    }
    Ok(())
}

use crate::scan::bootstrap::ListStorage;
use crate::scan::mapping::OwnedRun;
use adamite_fixtures::{
    Fixture,
    builders::{
        AttrBody, AttrBuilder, AttrListBuilder, AttrListEntryModel, BootSectorBuilder,
        NonResidentModel, RecordBuilder,
    },
};
use adamite_ntfs::{AttributeType, FileRecord, fixup};
use std::path::Path;

type TestResult = Result<(), Box<dyn Error>>;
type DriveOutcome = (Vec<ReadRequest>, Result<(), ScanError>);
type DiscoveryOutcome = (Scanner, Vec<ReadRequest>, Result<(), ScanError>);

fn boot() -> BootSectorBuilder {
    BootSectorBuilder {
        sectors: 256,
        ..BootSectorBuilder::default()
    }
}

fn data() -> AttrBuilder {
    AttrBuilder::non_resident(
        0x80,
        NonResidentModel {
            highest: 3,
            sizes: [16_384, 16_384, 16_384, 0],
            mapping_pairs: vec![0x11, 4, 4, 0],
            ..NonResidentModel::default()
        },
    )
}

fn record() -> RecordBuilder {
    RecordBuilder {
        record_number: 0,
        attributes: vec![data().build()],
        ..RecordBuilder::default()
    }
}

fn image(boot: &BootSectorBuilder, record: &[u8]) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut image = vec![0; 131_072];
    image
        .get_mut(..512)
        .ok_or("boot image slice")?
        .copy_from_slice(&boot.build());
    let geometry = parse_boot_sector(&boot.build())?;
    let offset = geometry
        .mft_lcn()
        .checked_mul(u64::from(geometry.cluster_size()))
        .ok_or("record offset overflow")?;
    let start = usize::try_from(offset)?;
    let end = start
        .checked_add(record.len())
        .ok_or("record end overflow")?;
    image
        .get_mut(start..end)
        .ok_or("record image slice")?
        .copy_from_slice(record);
    Ok(image)
}

/// Hold requests until the driver reports waiting, then respond in reverse order.
fn drive(scanner: &mut Scanner, image: &[u8]) -> Result<DriveOutcome, Box<dyn Error>> {
    drive_to(scanner, image, ScanPhase::Discover)
}

fn drive_to(
    scanner: &mut Scanner,
    image: &[u8],
    stop: ScanPhase,
) -> Result<DriveOutcome, Box<dyn Error>> {
    let mut requests = Vec::new();
    let mut pending = Vec::new();
    for _ in 0..4096 {
        match scanner.step() {
            Ok(ScanStep::Need(request)) => {
                assert!(request.len <= scanner.config.chunk_size);
                requests.push(request);
                pending.push(request);
                assert!(pending.len() <= scanner.config.max_outstanding);
            }
            Ok(ScanStep::Progress(progress)) => {
                if progress.phase == stop {
                    return Ok((requests, Ok(())));
                }
                assert_eq!(
                    progress.records_processed, 0,
                    "bootstrap must not publish or count records"
                );
                if progress.waiting_for_input {
                    assert!(
                        !pending.is_empty(),
                        "waiting without an outstanding request"
                    );
                    for request in pending.drain(..).rev() {
                        let start = usize::try_from(request.offset)?;
                        let end = start
                            .checked_add(request.len)
                            .ok_or("response end overflow")?;
                        scanner.feed(
                            request.id,
                            image.get(start..end).ok_or("response image slice")?,
                        )?;
                    }
                }
            }
            Err(error) => return Ok((requests, Err(error))),
            Ok(ScanStep::Done(_)) => return Err("bootstrap unexpectedly completed the scan".into()),
        }
    }
    Err("bootstrap did not reach a terminal boundary".into())
}

fn reject_record(record: &[u8]) -> Result<ScanError, Box<dyn Error>> {
    let mut scanner = Scanner::new(ScanConfig::default())?;
    let (_, outcome) = drive(&mut scanner, &image(&boot(), record)?)?;
    let error = outcome.err().ok_or("expected record-zero failure")?;
    assert!(
        scanner.bootstrap.is_none(),
        "invalid bootstrap must not be published"
    );
    assert!(
        scanner.record_read.is_none(),
        "failed scan retains raw record assembly"
    );
    assert!(matches!(scanner.step(), Err(ScanError::Inactive)));
    Ok(error)
}

#[test]
fn bootstrap_record0_short_boot_response_is_rejected() -> TestResult {
    let mut scanner = Scanner::new(ScanConfig::default())?;
    let ScanStep::Need(request) = scanner.step()? else {
        return Err("expected boot request".into());
    };
    assert!(matches!(
        scanner.feed(request.id, &[0; 511]),
        Err(FeedError::LengthMismatch { .. })
    ));
    let ScanStep::Progress(progress) = scanner.step()? else {
        return Err("expected waiting".into());
    };
    assert!(progress.waiting_for_input);
    scanner.feed(request.id, &boot().build())?;
    assert!(matches!(
        scanner.step()?,
        ScanStep::Progress(ScanProgress {
            phase: ScanPhase::Record0,
            ..
        })
    ));
    Ok(())
}

#[test]
fn bootstrap_record0_invalid_geometry_and_mft_locations() -> TestResult {
    let cases = [
        BootSectorBuilder {
            sector: 0,
            ..boot()
        },
        BootSectorBuilder {
            cluster_encoding: 0,
            ..boot()
        },
        BootSectorBuilder {
            file_encoding: 0,
            ..boot()
        },
        BootSectorBuilder {
            sectors: u64::MAX,
            ..boot()
        },
        BootSectorBuilder {
            sectors: i64::MAX as u64,
            ..boot()
        },
        BootSectorBuilder { mft: 32, ..boot() },
        BootSectorBuilder {
            mft: u64::MAX,
            ..boot()
        },
        BootSectorBuilder {
            mirror: 32,
            ..boot()
        },
        // A valid starting LCN is insufficient when the record crosses the volume end.
        BootSectorBuilder {
            cluster_encoding: 1,
            file_encoding: -12,
            mft: 255,
            ..boot()
        },
    ];
    for boot in cases {
        let mut scanner = Scanner::new(ScanConfig::default())?;
        let (requests, error) = drive(&mut scanner, &boot.build())?;
        assert_eq!(requests.len(), 1);
        assert!(
            matches!(error, Err(ScanError::Geometry { .. })),
            "{error:?}"
        );
        assert!(scanner.bootstrap.is_none());
        assert!(matches!(scanner.step(), Err(ScanError::Inactive)));
    }
    let mut invalid = boot().build();
    invalid.get_mut(3..11).ok_or("OEM field")?.fill(0);
    let mut scanner = Scanner::new(ScanConfig::default())?;
    assert!(matches!(
        drive(&mut scanner, &invalid)?.1,
        Err(ScanError::Geometry { .. })
    ));
    Ok(())
}

#[test]
fn bootstrap_record0_requires_in_use_base_identity_and_sequence() -> TestResult {
    for builder in [
        RecordBuilder {
            flags: 0,
            ..record()
        },
        RecordBuilder {
            base: (7u64 << 48) | 3,
            ..record()
        },
        RecordBuilder {
            sequence: 0,
            ..record()
        },
        RecordBuilder {
            record_number: 1,
            ..record()
        },
    ] {
        assert!(matches!(
            reject_record(&builder.build())?,
            ScanError::Bootstrap { .. }
        ));
    }
    Ok(())
}

#[test]
fn bootstrap_record0_missing_unnamed_or_first_data_segment() -> TestResult {
    let mut named = data();
    named.name = vec![65];
    let continuation = AttrBuilder::non_resident(
        0x80,
        NonResidentModel {
            lowest: 1,
            highest: 1,
            mapping_pairs: vec![0x11, 1, 4, 0],
            ..NonResidentModel::default()
        },
    );
    for attributes in [vec![], vec![named.build()], vec![continuation.build()]] {
        assert!(matches!(
            reject_record(
                &RecordBuilder {
                    attributes,
                    ..record()
                }
                .build()
            )?,
            ScanError::Bootstrap { .. }
        ));
    }
    Ok(())
}

#[test]
fn bootstrap_record0_resident_and_unsupported_data() -> TestResult {
    let resident = AttrBuilder::resident(0x80, vec![1, 2, 3]);
    let mut sparse = data();
    sparse.flags = 0x8000;
    let mut compressed = data();
    compressed.flags = 1;
    let mut encrypted = data();
    encrypted.flags = 0x4000;
    let sparse_run = AttrBuilder::non_resident(
        0x80,
        NonResidentModel {
            highest: 3,
            sizes: [16_384, 16_384, 0, 0],
            mapping_pairs: vec![0x01, 4, 0],
            ..NonResidentModel::default()
        },
    );
    for attr in [resident, sparse, compressed, encrypted, sparse_run] {
        assert!(matches!(
            reject_record(
                &RecordBuilder {
                    attributes: vec![attr.build()],
                    ..record()
                }
                .build()
            )?,
            ScanError::Bootstrap { .. }
        ));
    }
    Ok(())
}

#[test]
fn bootstrap_record0_zero_and_unaligned_data_size() -> TestResult {
    for size in [0, 1025] {
        let attr = AttrBuilder::non_resident(
            0x80,
            NonResidentModel {
                highest: 3,
                sizes: [16_384, size, 0, 0],
                mapping_pairs: vec![0x11, 4, 4, 0],
                ..NonResidentModel::default()
            },
        );
        assert!(matches!(
            reject_record(
                &RecordBuilder {
                    attributes: vec![attr.build()],
                    ..record()
                }
                .build()
            )?,
            ScanError::Bootstrap { .. }
        ));
    }
    Ok(())
}

#[test]
fn bootstrap_record0_exhausts_runlist_and_attribute_iterators() -> TestResult {
    // Each runlist yields a valid first run before detecting the late error.
    for pairs in [vec![0x11, 3, 4, 0], vec![0x11, 4, 4, 0x11, 0, 0]] {
        let attr = AttrBuilder::non_resident(
            0x80,
            NonResidentModel {
                highest: 3,
                sizes: [16_384, 16_384, 0, 0],
                mapping_pairs: pairs,
                ..NonResidentModel::default()
            },
        );
        assert!(matches!(
            reject_record(
                &RecordBuilder {
                    attributes: vec![attr.build()],
                    ..record()
                }
                .build()
            )?,
            ScanError::Runlist { .. }
        ));
    }
    let mut invalid = AttrBuilder::resident(0x10, vec![]).build();
    invalid.get_mut(..4).ok_or("attribute type")?.fill(0);
    assert!(matches!(
        reject_record(
            &RecordBuilder {
                attributes: vec![data().build(), invalid],
                ..record()
            }
            .build()
        )?,
        ScanError::Attribute { .. }
    ));
    assert!(matches!(
        reject_record(
            &RecordBuilder {
                attributes: vec![data().build(), data().build()],
                ..record()
            }
            .build()
        )?,
        ScanError::Bootstrap { .. }
    ));
    Ok(())
}

#[test]
fn bootstrap_record0_rejects_out_of_volume_runs_and_wrong_self_mapping() -> TestResult {
    let attr = AttrBuilder::non_resident(
        0x80,
        NonResidentModel {
            highest: 3,
            sizes: [16_384, 16_384, 0, 0],
            mapping_pairs: vec![0x11, 4, 31, 0],
            ..NonResidentModel::default()
        },
    );
    assert!(matches!(
        reject_record(
            &RecordBuilder {
                attributes: vec![attr.build()],
                ..record()
            }
            .build()
        )?,
        ScanError::OutOfVolume { .. }
    ));
    let attr = AttrBuilder::non_resident(
        0x80,
        NonResidentModel {
            highest: 3,
            sizes: [16_384, 16_384, 0, 0],
            mapping_pairs: vec![0x11, 4, 5, 0],
            ..NonResidentModel::default()
        },
    );
    assert!(matches!(
        reject_record(
            &RecordBuilder {
                attributes: vec![attr.build()],
                ..record()
            }
            .build()
        )?,
        ScanError::UnmappedRange { .. }
    ));
    Ok(())
}

#[test]
fn bootstrap_record0_split_records_and_reordered_responses() -> TestResult {
    for (encoding, size) in [(-10, 1024), (-12, 4096)] {
        for max_outstanding in [1, 3, 8] {
            let boot = BootSectorBuilder {
                file_encoding: encoding,
                ..boot()
            };
            let builder = RecordBuilder { size, ..record() };
            let mut scanner = Scanner::new(ScanConfig {
                max_outstanding,
                chunk_size: 512,
                ..ScanConfig::default()
            })?;
            let (requests, error) = drive(&mut scanner, &image(&boot, &builder.build())?)?;
            error?;
            assert_eq!(
                requests.len(),
                size.checked_div(512)
                    .and_then(|n| n.checked_add(1))
                    .ok_or("request count")?
            );
            assert!(requests.iter().all(|request| request.len == 512));
            let record0 = scanner
                .bootstrap
                .as_ref()
                .ok_or("missing decoded record zero")?;
            assert_eq!(record0.reference.record(), 0);
            assert_eq!(record0.reference.sequence(), builder.sequence);
            assert_eq!(record0.data_size, 16_384);
            assert_eq!(
                record0.mapping.runs,
                vec![OwnedRun {
                    vcn: 0,
                    lcn: 4,
                    clusters: 4
                }]
            );
            assert!(record0.attribute_lists.is_empty());
            assert_eq!(scanner.phase, ScanPhase::Discover);
        }
    }
    Ok(())
}

#[test]
fn bootstrap_record0_retains_resident_and_nonresident_attribute_list_descriptors() -> TestResult {
    let value = AttrListBuilder {
        entries: vec![AttrListEntryModel::default()],
        zero_tail: 0,
    }
    .build();
    let mut resident = AttrBuilder::resident(0x20, value.clone());
    resident.id = 11;
    let mut nonresident = AttrBuilder::non_resident(0x20, NonResidentModel::default());
    nonresident.id = 12;
    let bytes = RecordBuilder {
        attributes: vec![resident.build(), data().build(), nonresident.build()],
        ..record()
    }
    .build();
    let mut scanner = Scanner::new(ScanConfig::default())?;
    let (_, error) = drive(&mut scanner, &image(&boot(), &bytes)?)?;
    error?;
    let record0 = scanner.bootstrap.as_ref().ok_or("record-zero handoff")?;
    let mut lists = record0.attribute_lists.iter();
    let resident = lists.next().ok_or("resident list")?;
    assert_eq!(resident.id, 11);
    assert!(resident.name.is_empty());
    assert_eq!(resident.flags.raw(), 0);
    assert_eq!(resident.storage, ListStorage::Resident(value));
    let nonresident = lists.next().ok_or("nonresident list")?;
    assert_eq!(nonresident.id, 12);
    match &nonresident.storage {
        ListStorage::NonResident {
            lowest_vcn,
            highest_vcn,
            compression_unit,
            sizes,
            mapping_pairs,
        } => {
            assert_eq!((*lowest_vcn, *highest_vcn, *compression_unit), (0, 0, 0));
            assert_eq!(sizes.ok_or("list sizes")?.data(), 4096);
            assert!(mapping_pairs.starts_with(&[0x11, 1, 1, 0]));
        }
        other => return Err(format!("expected nonresident list, got {other:?}").into()),
    }
    assert!(lists.next().is_none());
    Ok(())
}

#[test]
fn bootstrap_record0_no_mirror_fallback_on_corrupt_primary() -> TestResult {
    let builder = record();
    let good = builder.build();
    let mut bad = good.clone();
    builder.corrupt_tail(&mut bad, 0);
    let mut image = image(&boot(), &bad)?;
    image
        .get_mut(32_768..33_792)
        .ok_or("mirror slice")?
        .copy_from_slice(&good);
    let mut scanner = Scanner::new(ScanConfig::default())?;
    let (requests, error) = drive(&mut scanner, &image)?;
    assert!(matches!(error, Err(ScanError::Fixup { .. })));
    assert_eq!(
        requests
            .iter()
            .map(|request| request.offset)
            .collect::<Vec<_>>(),
        vec![0, 16_384]
    );
    assert!(scanner.bootstrap.is_none());
    Ok(())
}

fn real_fixture(name: &str) -> TestResult {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/ntfs")
        .join(format!("{name}.json"));
    let fixture = Fixture::load(&path)?;
    let manifest = fixture.manifest();
    assert_eq!(manifest.schema_version, 3);
    let mut scanner = Scanner::new(ScanConfig {
        max_outstanding: 3,
        chunk_size: 512,
        ..ScanConfig::default()
    })?;
    let (_, error) = drive(&mut scanner, fixture.image())?;
    error?;
    let record0 = scanner
        .bootstrap
        .as_ref()
        .ok_or("fixture record-zero handoff")?;
    let geometry = record0.mapping.geometry;
    let expected = &manifest.geometry;
    assert_eq!(geometry.bytes_per_sector(), expected.bytes_per_sector);
    assert_eq!(geometry.cluster_size(), expected.cluster_size);
    assert_eq!(geometry.file_record_size(), expected.file_record_size);
    assert_eq!(geometry.index_record_size(), expected.index_record_size);
    assert_eq!(geometry.total_clusters(), expected.total_clusters);
    assert_eq!(geometry.mft_lcn(), expected.mft_lcn);
    assert_eq!(geometry.mftmirr_lcn(), expected.mftmirr_lcn);
    assert_eq!(record0.data_size, manifest.mft.data_size);
    assert_eq!(
        !record0.attribute_lists.is_empty(),
        manifest.mft.has_attribute_list
    );

    // Read only the header's first-segment boundary independently of core state.
    let start = usize::try_from(
        expected
            .mft_lcn
            .checked_mul(u64::from(expected.cluster_size))
            .ok_or("fixture offset")?,
    )?;
    let end = start
        .checked_add(usize::try_from(expected.file_record_size)?)
        .ok_or("fixture record end")?;
    let mut raw = fixture
        .image()
        .get(start..end)
        .ok_or("fixture record slice")?
        .to_vec();
    let fixed = fixup::apply(&mut raw)?;
    let record = FileRecord::parse(fixed)?;
    let mut highest = None;
    for attr in record.attributes() {
        let attr = attr?;
        if attr.attribute_type() == AttributeType::DATA
            && attr.name().len_units() == 0
            && let Some(segment) = attr.non_resident()
            && segment.lowest_vcn() == 0
        {
            highest = Some(u64::try_from(segment.highest_vcn())?);
        }
    }
    let highest = highest.ok_or("fixture first segment")?;
    let expected_runs = manifest
        .mft
        .extents
        .iter()
        .take_while(|extent| extent.vcn <= highest)
        .map(|extent| {
            Ok(OwnedRun {
                vcn: extent.vcn,
                lcn: extent.lcn.ok_or("sparse fixture MFT")?,
                clusters: extent.len,
            })
        })
        .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
    assert_eq!(
        record0.mapping.runs, expected_runs,
        "{name}: first-segment manifest extents"
    );
    if name == "frag-512" {
        assert!(
            expected_runs.len() < manifest.mft.extents.len(),
            "extension discovery must remain pending"
        );
    } else {
        assert_eq!(expected_runs.len(), manifest.mft.extents.len());
    }
    Ok(())
}

#[test]
fn bootstrap_record0_fixture_basic_4k() -> TestResult {
    real_fixture("basic-4k")
}
#[test]
fn bootstrap_record0_fixture_frag_512_first_segment() -> TestResult {
    real_fixture("frag-512")
}
#[test]
fn bootstrap_record0_fixture_native_4kn_64k() -> TestResult {
    real_fixture("4kn-64k")
}

use adamite_fixtures::runlist_encoder::{RunModel, RunlistEncoder};

fn segment(id: u16, lowest: i64, highest: i64, lcn: u64, len: u64) -> AttrBuilder {
    let mut attribute = AttrBuilder::non_resident(
        0x80,
        NonResidentModel {
            lowest,
            highest,
            sizes: [6144, 6144, 6144, 0],
            mapping_pairs: RunlistEncoder {
                runs: vec![RunModel {
                    lcn: Some(lcn),
                    len,
                }],
            }
            .encode(0),
            ..NonResidentModel::default()
        },
    );
    attribute.id = id;
    attribute
}

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

fn put(image: &mut [u8], offset: usize, bytes: &[u8]) -> TestResult {
    let end = offset
        .checked_add(bytes.len())
        .ok_or("synthetic extent end")?;
    image
        .get_mut(offset..end)
        .ok_or("synthetic extent slice")?
        .copy_from_slice(bytes);
    Ok(())
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

fn discover_image(
    image: &[u8],
    cap: usize,
    chunk: usize,
) -> Result<DiscoveryOutcome, Box<dyn Error>> {
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
            let (mut scanner, requests, outcome) = discover_image(&image, cap, chunk)?;
            outcome?;
            let base = scanner.bootstrap.as_ref().ok_or("complete MFT mapping")?;
            assert_eq!(base.mapping.runs, chain_runs());
            assert_eq!(base.data_size, 6144);
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
            assert!(matches!(
                scanner.step(),
                Err(ScanError::PhaseNotImplemented {
                    phase: ScanPhase::Stream
                })
            ));
            assert!(matches!(scanner.step(), Err(ScanError::Inactive)));
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
    assert!(matches!(outcome, Err(ScanError::Fixup { .. })));
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
        let (mut scanner, _, outcome) = discover_image(fixture.image(), cap, chunk)?;
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
        assert!(matches!(
            scanner.step(),
            Err(ScanError::PhaseNotImplemented {
                phase: ScanPhase::Stream
            })
        ));
        assert!(matches!(scanner.step(), Err(ScanError::Inactive)));
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
