use super::*;
use crate::scan::ReadRequest;
use adamite_ntfs::record::{RecordError, fixup::FixupError};
use std::error::Error;

#[test]
fn protocol_done_transfers_output_once_then_rejects_calls() -> Result<(), Box<dyn Error>> {
    let mut scanner = Scanner::new(ScanConfig::default())?;
    let fixture = Fixture::load(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/ntfs/basic-4k.json"),
    )?;
    let (_, outcome) = drive_to(&mut scanner, fixture.image(), ScanPhase::Aggregate)?;
    outcome?;
    let (_, outcome) = drive_to(&mut scanner, fixture.image(), ScanPhase::Done)?;
    outcome?;
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

use crate::mft::bootstrap::{ListStorage, RecordParts};
use crate::mft::mapping::OwnedRun;
use adamite_fixtures::{
    Fixture,
    builders::{
        AttrBody, AttrBuilder, AttrListBuilder, AttrListEntryModel, BootSectorBuilder,
        NonResidentModel, RecordBuilder,
    },
};
use adamite_ntfs::{
    attribute::AttributeType,
    record::{FileRecord, reference::FileRef},
};
use std::path::Path;

type TestResult = Result<(), Box<dyn Error>>;
type ObservedDriveOutcome = (Vec<ReadRequest>, Result<(), ScanError>, usize);
type DriveOutcome = (Vec<ReadRequest>, Result<(), ScanError>);
type DriverOutcome = (Scanner, Vec<ReadRequest>, Result<(), ScanError>);

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
    let geometry = VolumeGeometry::from_boot_sector(&boot.build())?;
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
    let (requests, outcome, _) = drive_to_observed(scanner, image, stop)?;
    Ok((requests, outcome))
}

fn drive_to_observed(
    scanner: &mut Scanner,
    image: &[u8],
    stop: ScanPhase,
) -> Result<ObservedDriveOutcome, Box<dyn Error>> {
    let mut peak_transport_bytes = 0usize;
    let mut requests = Vec::new();
    let mut pending = Vec::new();
    let calls = image
        .len()
        .checked_div(512)
        .and_then(|n| n.checked_mul(16))
        .and_then(|n| n.checked_add(4096))
        .ok_or("driver step bound")?;
    let mut previous_records = scanner
        .ingest
        .as_ref()
        .map_or(0, |state| state.stats.records_total);
    for _ in 0..calls {
        let result = scanner.step();
        peak_transport_bytes = peak_transport_bytes.max(assert_transport_bound(scanner)?);
        match result {
            Ok(ScanStep::Need(request)) => {
                assert!(request.len <= scanner.config.chunk_size);
                requests.push(request);
                pending.push(request);
                assert!(pending.len() <= scanner.config.max_outstanding);
            }
            Ok(ScanStep::Progress(progress)) => {
                let processed = progress
                    .records_processed
                    .checked_sub(previous_records)
                    .ok_or("progress moved backwards")?;
                assert!(processed <= 256, "CPU step processed too many records");
                previous_records = progress.records_processed;
                if progress.phase == stop {
                    return Ok((requests, Ok(()), peak_transport_bytes));
                }
                if matches!(
                    progress.phase,
                    ScanPhase::Boot | ScanPhase::Record0 | ScanPhase::Discover
                ) {
                    assert_eq!(
                        progress.records_processed, 0,
                        "bootstrap must not count records"
                    );
                }
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
                        peak_transport_bytes =
                            peak_transport_bytes.max(assert_transport_bound(scanner)?);
                    }
                }
            }
            Err(error) => return Ok((requests, Err(error), peak_transport_bytes)),
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
fn bootstrap_record_and_extension_headers_precede_fixup_with_preserved_sources() -> TestResult {
    let geometry = VolumeGeometry::from_boot_sector(&boot().build())?;
    let base = FileRef::new(0, 7)?;
    let extension_ref = FileRef::new(1, 7)?;
    for extension in [false, true] {
        for invalid_header in [false, true] {
            for torn in [false, true] {
                let builder = if extension {
                    RecordBuilder {
                        record_number: 1,
                        base: base.raw(),
                        ..record()
                    }
                } else {
                    record()
                };
                let mut raw = builder.build();
                if invalid_header {
                    raw.get_mut(28..32)
                        .ok_or("allocated-size field")?
                        .copy_from_slice(&999u32.to_le_bytes());
                }
                if torn {
                    builder.corrupt_tail(&mut raw, 1);
                }
                let result = if extension {
                    RecordParts::parse_extension(raw, extension_ref, base, geometry).map(|_| ())
                } else {
                    Record0::parse(raw, geometry).map(|_| ())
                };
                if !invalid_header && !torn {
                    result?;
                    continue;
                }
                let error = result.err().ok_or("expected bootstrap parse failure")?;
                let nested = error.source().ok_or("missing parser source")?;
                match &error {
                    ScanError::Record { source } => {
                        assert!(invalid_header);
                        assert_eq!(
                            *source,
                            RecordError::InvalidAllocatedSize {
                                declared: 999,
                                actual: 1024
                            }
                        );
                        assert_eq!(nested.downcast_ref::<RecordError>(), Some(source));
                        assert_eq!(
                            error.to_string(),
                            format!("invalid bootstrap record: {source}")
                        );
                    }
                    ScanError::Fixup { source } => {
                        assert!(!invalid_header && torn);
                        assert!(matches!(
                            source,
                            FixupError::TrailerMismatch { stride: 1, .. }
                        ));
                        assert_eq!(nested.downcast_ref::<FixupError>(), Some(source));
                        assert_eq!(
                            error.to_string(),
                            format!("invalid bootstrap fixup: {source}")
                        );
                    }
                    _ => return Err(error.into()),
                }
                assert!(nested.source().is_none());
            }
        }
    }
    Ok(())
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
    let record = FileRecord::parse(&mut raw)?;
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

use crate::mft::model::{RecordKind, StreamStorage};
use adamite_fixtures::builders::FileNameBuilder;

fn assert_transport_bound(scanner: &Scanner) -> Result<usize, Box<dyn Error>> {
    if let Some(state) = &scanner.ingest {
        let record_size = usize::try_from(
            scanner
                .bootstrap
                .as_ref()
                .ok_or("stream mapping")?
                .mapping
                .geometry
                .file_record_size(),
        )?;
        let bound = scanner
            .config
            .max_outstanding
            .checked_mul(scanner.config.chunk_size)
            .and_then(|n| n.checked_add(record_size))
            .ok_or("transport byte bound")?;
        let retained = Scanner::stream_transport_bytes(state, &scanner.requests)?;
        assert!(
            retained <= bound,
            "retained raw MFT exceeds cap * chunk + record"
        );
        return Ok(retained);
    }
    Ok(0)
}

fn named_record(number: u32, name: Vec<u16>) -> RecordBuilder {
    let filename = FileNameBuilder {
        name,
        parent: (7u64 << 48) | 5,
        ..FileNameBuilder::default()
    };
    RecordBuilder {
        record_number: number,
        attributes: vec![AttrBuilder::resident(0x30, filename.build()).build()],
        ..RecordBuilder::default()
    }
}

fn ingest_volume(count: u64, records: Vec<(usize, Vec<u8>)>) -> Result<Vec<u8>, Box<dyn Error>> {
    let boot = BootSectorBuilder {
        sectors: 2048,
        cluster_encoding: 1,
        mft: 8,
        mirror: 1900,
        ..boot()
    };
    let size = count.checked_mul(1024).ok_or("synthetic MFT length")?;
    let clusters = size.checked_div(512).ok_or("synthetic MFT clusters")?;
    let highest = i64::try_from(clusters.checked_sub(1).ok_or("last synthetic MFT VCN")?)?;
    let mut first = segment(0, 0, highest, 8, clusters);
    let AttrBody::NonResident(model) = &mut first.body else {
        return Err("MFT descriptor".into());
    };
    let size_i64 = i64::try_from(size)?;
    model.sizes = [size_i64, size_i64, size_i64, 0];
    let base = RecordBuilder {
        attributes: vec![first.build()],
        ..record()
    };
    let mut volume = image(&boot, &base.build())?;
    volume.resize(1_048_576, 0);
    let root = RecordBuilder {
        flags: 3,
        ..named_record(5, vec![46])
    };
    put(&mut volume, 9216, &root.build())?;
    for (ordinal, bytes) in records {
        let offset = ordinal
            .checked_mul(1024)
            .and_then(|n| n.checked_add(4096))
            .ok_or("synthetic record position")?;
        put(&mut volume, offset, &bytes)?;
    }
    Ok(volume)
}

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
    assert!(state.slots.get(7).ok_or("extension slot")?.base.is_none());
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
    assert!(state.slots.get(6).ok_or("bad base")?.base.is_none());
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
    assert!(state.slots.get(6).ok_or("mismatched slot")?.base.is_none());
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
            .get(13)
            .and_then(|slot| slot.base.as_ref())
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
    let contribution = state
        .slots
        .get(6)
        .and_then(|slot| slot.base.as_ref())
        .ok_or("typed contribution")?;
    let standard = contribution
        .standard_information
        .first()
        .ok_or("standard information")?;
    assert_eq!(contribution.standard_information.len(), 1);
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
        run_count,
        ..
    } = ads.storage
    else {
        return Err("ADS storage".into());
    };
    assert_eq!((lowest_vcn, run_count), (0, 1));
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
        assert_eq!(slot.sequence, Some(entry.seq), "{name}: {}", entry.path);
        let base = slot.base.as_ref().ok_or_else(|| {
            format!(
                "{name}: {} has no base contribution; stats {:?}",
                entry.path, state.stats
            )
        })?;
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
        assert!(slot.base.is_none());
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
            slot.base
                .as_ref()
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

use crate::{graph::entry::EntryRecord, snapshot::NodeFlags};

fn merge_image(image: &[u8], cap: usize, chunk: usize) -> Result<DriverOutcome, Box<dyn Error>> {
    let mut scanner = Scanner::new(ScanConfig {
        max_outstanding: cap,
        chunk_size: chunk,
        ..ScanConfig::default()
    })?;
    let (requests, outcome) = drive_to(&mut scanner, image, ScanPhase::Link)?;
    Ok((scanner, requests, outcome))
}

fn merged_entry(scanner: &Scanner, record: u64) -> Result<&EntryRecord, Box<dyn Error>> {
    scanner
        .entries
        .iter()
        .find(|entry| entry.file_ref.record() == record)
        .ok_or_else(|| {
            format!(
                "missing merged entry {record}; stats {:?}",
                scanner.ingest.as_ref().map(|ingest| &ingest.stats)
            )
            .into()
        })
}

#[test]
fn merge_driver_extension_before_base_ignores_cached_filename_sizes() -> TestResult {
    let first = AttrBuilder::non_resident(
        0x80,
        NonResidentModel {
            sizes: [1024, 1024, 1024, 0],
            ..NonResidentModel::default()
        },
    );
    let mut continuation = AttrBuilder::non_resident(
        0x80,
        NonResidentModel {
            lowest: 1,
            highest: 1,
            sizes: [-1; 4],
            ..NonResidentModel::default()
        },
    );
    continuation.id = 9;
    let mut base = named_record(6, vec![65]);
    base.attributes.push(first.build());
    base.hard_links = 2;
    let mut extension = RecordBuilder {
        base: (7u64 << 48) | 6,
        ..named_record(1, vec![66])
    };
    extension.attributes.push(continuation.build());
    let volume = ingest_volume(16, vec![(1, extension.build()), (6, base.build())])?;
    let (scanner, _, outcome) = merge_image(&volume, 4, 512)?;
    outcome?;
    let entry = merged_entry(&scanner, 6)?;
    assert_eq!((entry.size, entry.ads_size, entry.alloc), (1024, 0, 1024));
    assert_eq!(entry.nlink, 2);
    assert_eq!(entry.names.len(), 2);
    assert!(entry.names.iter().any(|name| name.name == vec![66]));
    assert_eq!(
        scanner.ingest.as_ref().ok_or("stats")?.stats.records_total,
        16
    );
    assert!(scanner.ingest.as_ref().ok_or("handoff")?.slots.is_empty());
    assert!(scanner.ingest.as_ref().ok_or("handoff")?.patches.is_empty());
    assert_eq!(scanner.phase, ScanPhase::Link);
    Ok(())
}

fn merge_fixture(name: &str) -> TestResult {
    let fixture = Fixture::load(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/ntfs")
            .join(format!("{name}.json")),
    )?;
    let (scanner, _, outcome) = merge_image(fixture.image(), 4, 512)?;
    outcome?;
    let manifest = fixture.manifest();
    let mut expected_records = std::collections::BTreeSet::new();
    for expected in manifest.entries.iter().chain(&manifest.metafiles) {
        expected_records.insert(expected.record);
        let actual = merged_entry(&scanner, expected.record)?;
        let mut size = 0u64;
        let mut ads_size = 0u64;
        let mut alloc = 0u64;
        for stream in &expected.streams {
            if stream.attribute_type == 0x80 {
                if stream.name.is_empty() {
                    size = stream.size;
                } else if !(expected.record == 8 && stream.name == [36, 66, 97, 100]) {
                    ads_size = ads_size
                        .checked_add(stream.size)
                        .ok_or("manifest ADS total")?;
                }
            }
            if !stream.resident {
                let measured = stream
                    .compressed_size
                    .or(stream.allocated_size)
                    .ok_or("v3 stream allocation")?;
                alloc = alloc
                    .checked_add(measured)
                    .ok_or("manifest allocation total")?;
            }
        }
        assert_eq!(
            (actual.size, actual.ads_size, actual.alloc),
            (size, ads_size, alloc),
            "{name}: {} (record {})",
            expected.path,
            expected.record
        );
        assert_eq!(
            u64::from(actual.nlink),
            expected.nlink,
            "{name}: {} nlink",
            expected.path
        );
        assert_eq!(
            actual.reparse_tag, expected.reparse_tag,
            "{name}: {} reparse tag",
            expected.path
        );
        assert_eq!(
            actual.attrs, expected.attributes,
            "{name}: {} SI attrs",
            expected.path
        );
        assert_eq!(
            actual.mtime, expected.mtime_filetime,
            "{name}: {} mtime",
            expected.path
        );
        assert_eq!(
            actual.file_ref.sequence(),
            expected.seq,
            "{name}: {} sequence",
            expected.path
        );
        let flags = [
            (
                NodeFlags::DIRECTORY,
                expected.attributes & 0x10 != 0
                    || matches!(expected.kind, adamite_fixtures::EntryKind::Directory),
            ),
            (
                NodeFlags::REPARSE,
                expected.attributes & 0x400 != 0 || expected.reparse_tag.is_some(),
            ),
            (
                NodeFlags::SPARSE,
                expected.attributes & 0x200 != 0
                    || expected.streams.iter().any(|stream| stream.sparse),
            ),
            (
                NodeFlags::COMPRESSED,
                expected.attributes & 0x800 != 0
                    || expected.streams.iter().any(|stream| stream.compressed),
            ),
            (NodeFlags::HIDDEN, expected.attributes & 2 != 0),
            (NodeFlags::SYSTEM, expected.attributes & 4 != 0),
        ];
        for (flag, present) in flags {
            assert_eq!(
                actual.flags.contains(flag),
                present,
                "{name}: {} flag {flag:?}",
                expected.path
            );
        }
        for name in &expected.names {
            assert!(
                actual.names.iter().any(|actual| actual.name == name.name
                    && actual.namespace == name.namespace
                    && actual.parent.record() == name.parent_record
                    && actual.parent.sequence() == name.parent_seq),
                "missing merged name for {}",
                expected.path
            );
        }
    }
    for fill in &manifest.fill {
        let prefix: Vec<u16> = fill.name_prefix.encode_utf16().collect();
        let entries: Vec<_> = scanner
            .entries
            .iter()
            .filter(|entry| {
                (fill.record_min..=fill.record_max).contains(&entry.file_ref.record())
                    && entry.names.iter().any(|name| {
                        name.parent.record() == fill.parent_record
                            && name.parent.sequence() == fill.parent_seq
                            && name.name.len() == fill.name_len
                            && name.name.starts_with(&prefix)
                    })
            })
            .collect();
        assert_eq!(
            entries.len(),
            fill.count,
            "{name}: {} filler count",
            fill.dir
        );
        let mut totals = (0u64, 0u64, 0u64);
        for entry in entries {
            assert!(
                expected_records.insert(entry.file_ref.record()),
                "filler coverage overlaps explicit or other filler entries"
            );
            totals.0 = totals
                .0
                .checked_add(entry.size)
                .ok_or("filler logical total")?;
            totals.1 = totals
                .1
                .checked_add(entry.ads_size)
                .ok_or("filler ADS total")?;
            totals.2 = totals
                .2
                .checked_add(entry.alloc)
                .ok_or("filler allocation total")?;
        }
        assert_eq!(
            (Some(totals.0), Some(totals.1), Some(totals.2)),
            (fill.total_size, fill.total_ads_size, fill.total_alloc),
            "{name}: {} filler measurements",
            fill.dir
        );
    }
    assert_eq!(
        scanner.entries.len(),
        expected_records.len(),
        "{name}: every surviving base is covered by the oracle"
    );
    let stats = &scanner.ingest.as_ref().ok_or("stats")?.stats;
    assert_eq!(stats.rejected_extensions, 0, "{name}: rejected patches");
    assert_eq!(stats.corrupt_streams, 0, "{name}: rejected streams");
    Ok(())
}

#[test]
fn merge_fixture_basic_4k_all_entries_and_metafiles() -> TestResult {
    merge_fixture("basic-4k")
}
#[test]
fn merge_fixture_frag_512_entries_metafiles_and_fill_totals() -> TestResult {
    merge_fixture("frag-512")
}
#[test]
fn merge_fixture_native_4kn_64k_all_entries_and_metafiles() -> TestResult {
    merge_fixture("4kn-64k")
}

use crate::snapshot::ParentTree;

fn name_bytes<'a>(
    tree: &'a ParentTree,
    node: &crate::snapshot::ResolvedNode,
) -> Result<&'a [u8], Box<dyn Error>> {
    let start = usize::try_from(node.name_off)?;
    let end = start
        .checked_add(usize::from(node.name_len))
        .ok_or("name range overflow")?;
    tree.names
        .get(start..end)
        .ok_or_else(|| "name pool range".into())
}

fn parent_path(tree: &ParentTree, mut id: usize) -> Result<String, Box<dyn Error>> {
    let mut parts = Vec::new();
    for _ in 0..tree.nodes.len() {
        if id == 0 {
            parts.reverse();
            return Ok(format!("/{}", parts.join("/")));
        }
        let node = tree.nodes.get(id).ok_or("parent-array index")?;
        parts.push(std::str::from_utf8(name_bytes(tree, node)?)?);
        id = node.parent;
    }
    Err("cycle in parent-array fixture path".into())
}

fn linking_fixture(name: &str) -> TestResult {
    let fixture = Fixture::load(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/ntfs")
            .join(format!("{name}.json")),
    )?;
    let mut scanner = Scanner::new(ScanConfig {
        max_outstanding: 4,
        chunk_size: 512,
        ..ScanConfig::default()
    })?;
    let (_, outcome) = drive_to(&mut scanner, fixture.image(), ScanPhase::Snapshot)?;
    outcome?;
    let tree = scanner
        .parent_tree
        .as_ref()
        .ok_or("flat parent-index array")?;
    let mut paths = std::collections::BTreeMap::new();
    for (id, node) in tree.nodes.iter().enumerate() {
        assert!(node.parent < tree.nodes.len());
        if id != 0 {
            assert!(
                !tree
                    .nodes
                    .get(node.parent)
                    .ok_or("parent")?
                    .flags
                    .contains(NodeFlags::HARDLINK_DUP),
                "children attached to secondary directory"
            );
        }
        paths.insert(parent_path(tree, id)?, id);
    }
    for expected in &fixture.manifest().entries {
        let id = paths
            .get(&expected.path)
            .ok_or_else(|| format!("{name}: missing path {}", expected.path))?;
        let node = tree.nodes.get(*id).ok_or("manifest node")?;
        assert_eq!(
            node.file_ref
                .map(|reference| (reference.record(), reference.sequence())),
            Some((expected.record, expected.seq)),
            "{name}: {}",
            expected.path
        );
    }
    let metadata = *paths.get("/<NTFS metadata>").ok_or("metadata directory")?;
    let expected_meta: std::collections::BTreeSet<_> = fixture
        .manifest()
        .metafiles
        .iter()
        .map(|entry| entry.record)
        .collect();
    for expected in &expected_meta {
        let nodes: Vec<_> = tree
            .nodes
            .iter()
            .filter(|node| {
                node.file_ref
                    .is_some_and(|reference| reference.record() == *expected)
            })
            .collect();
        assert!(!nodes.is_empty(), "missing metadata record {expected}");
        for node in nodes {
            assert_eq!(
                node.parent, metadata,
                "metadata record {expected} outside metadata directory"
            );
        }
    }
    let stats = &scanner.ingest.as_ref().ok_or("stats")?.stats;
    assert_eq!(stats.metafiles, u64::try_from(expected_meta.len())?);
    assert_eq!(
        stats.hardlink_duplicates,
        u64::try_from(
            tree.nodes
                .iter()
                .filter(|node| node.flags.contains(NodeFlags::HARDLINK_DUP))
                .count()
        )?
    );
    if name == "basic-4k" {
        let links: Vec<_> = tree
            .nodes
            .iter()
            .filter(|node| {
                node.file_ref
                    .is_some_and(|reference| reference.record() == 270)
            })
            .collect();
        assert_eq!(links.len(), 3);
        assert_eq!(
            links
                .iter()
                .filter(|node| !node.flags.contains(NodeFlags::HARDLINK_DUP))
                .count(),
            1
        );
        assert!(!paths.contains_key("/deleted.txt"));
        assert!(
            tree.nodes
                .iter()
                .all(|node| name_bytes(tree, node).is_ok_and(|name| name != b"LONGFI~1.TXT"))
        );
        let deepest = fixture
            .manifest()
            .entries
            .iter()
            .max_by_key(|entry| entry.path.matches('/').count())
            .ok_or("deep chain oracle")?;
        assert!(deepest.path.matches('/').count() >= 41);
        assert!(paths.contains_key(&deepest.path));
    }
    if name == "frag-512" {
        for fill in &fixture.manifest().fill {
            let directory = *paths.get(&fill.dir).ok_or("filler directory path")?;
            assert_eq!(
                tree.nodes
                    .iter()
                    .filter(|node| node.parent == directory)
                    .count(),
                fill.count,
                "{} child count",
                fill.dir
            );
        }
    }
    let (_, outcome) = drive_to(&mut scanner, fixture.image(), ScanPhase::Aggregate)?;
    outcome?;
    let (_, outcome) = drive_to(&mut scanner, fixture.image(), ScanPhase::Done)?;
    outcome?;
    assert!(matches!(scanner.step()?, ScanStep::Done(_)));
    assert!(matches!(scanner.step(), Err(ScanError::Inactive)));
    Ok(())
}

#[test]
fn linking_fixture_basic_4k_paths_hardlinks_aliases_and_deep_chain() -> TestResult {
    linking_fixture("basic-4k")
}
#[test]
fn linking_fixture_frag_512_paths_and_filler_child_counts() -> TestResult {
    linking_fixture("frag-512")
}
#[test]
fn linking_fixture_native_4kn_64k_paths_and_metadata() -> TestResult {
    linking_fixture("4kn-64k")
}

#[test]
fn linking_driver_counts_invalid_utf16_at_conversion_once() -> TestResult {
    let volume = ingest_volume(
        16,
        vec![(13, named_record(13, vec![0xd800, 65, 0xdc00]).build())],
    )?;
    let mut scanner = Scanner::new(ScanConfig::default())?;
    let (_, outcome) = drive_to(&mut scanner, &volume, ScanPhase::Merge)?;
    outcome?;
    assert_eq!(
        scanner
            .ingest
            .as_ref()
            .ok_or("pre-link stats")?
            .stats
            .invalid_utf16,
        0
    );
    let (_, outcome) = drive_to(&mut scanner, &volume, ScanPhase::Snapshot)?;
    outcome?;
    assert_eq!(
        scanner
            .ingest
            .as_ref()
            .ok_or("post-link stats")?
            .stats
            .invalid_utf16,
        2
    );
    Ok(())
}

fn snapshot_fixture(name: &str) -> TestResult {
    let fixture = Fixture::load(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/ntfs")
            .join(format!("{name}.json")),
    )?;
    let context = crate::scan::ScanContext {
        label: Some(name.into()),
        scanned_at_filetime: Some(123),
        free_bytes: Some(456),
    };
    let mut scanner = Scanner::new(ScanConfig {
        max_outstanding: 4,
        chunk_size: 512,
        context: context.clone(),
    })?;
    let (_, outcome) = drive_to(&mut scanner, fixture.image(), ScanPhase::Aggregate)?;
    outcome?;
    let output = scanner.output.as_ref().ok_or("snapshot output")?;
    let snapshot = &output.snapshot;
    assert_eq!(snapshot.volume().context(), &context);
    assert_eq!(&scanner.config.context, &context);
    let mut paths = std::collections::BTreeMap::new();
    for raw in 0..u32::try_from(snapshot.len())? {
        let id = crate::snapshot::NodeId::from_raw(raw);
        paths.insert(snapshot.path(id)?.ok_or("snapshot path")?, id);
    }
    for expected in &fixture.manifest().entries {
        let id = *paths
            .get(&expected.path)
            .ok_or_else(|| format!("{name}: missing {}", expected.path))?;
        let node = snapshot.node(id).ok_or("manifest node")?;
        assert_eq!(
            node.file_ref(),
            Some(FileRef::new(expected.record, expected.seq)?),
            "{name}: {}",
            expected.path
        );
        assert_eq!(node.attrs(), expected.attributes);
        assert_eq!(node.mtime(), expected.mtime_filetime);
        assert_eq!(u64::from(node.nlink()), expected.nlink);
        assert_eq!(node.reparse_tag(), expected.reparse_tag);
    }
    for expected in &fixture.manifest().metafiles {
        let leaf = expected
            .path
            .rsplit('/')
            .next()
            .ok_or("metafile basename")?;
        let path = format!("/<NTFS metadata>/{leaf}");
        let id = *paths
            .get(&path)
            .ok_or_else(|| format!("{name}: missing {path}"))?;
        assert_eq!(
            snapshot.node(id).ok_or("metafile node")?.file_ref(),
            Some(FileRef::new(expected.record, expected.seq)?)
        );
    }
    let (_, outcome) = drive_to(&mut scanner, fixture.image(), ScanPhase::Done)?;
    outcome?;
    assert!(matches!(scanner.step()?, ScanStep::Done(_)));
    assert!(matches!(scanner.step(), Err(ScanError::Inactive)));
    Ok(())
}

#[test]
fn snapshot_fixture_basic_4k_paths_and_file_refs() -> TestResult {
    snapshot_fixture("basic-4k")
}
#[test]
fn snapshot_fixture_frag_512_paths_and_file_refs() -> TestResult {
    snapshot_fixture("frag-512")
}
#[test]
fn snapshot_fixture_native_4kn_64k_paths_and_file_refs() -> TestResult {
    snapshot_fixture("4kn-64k")
}

fn aggregation_fixture(name: &str) -> TestResult {
    let fixture = Fixture::load(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/ntfs")
            .join(format!("{name}.json")),
    )?;
    let mut scanner = Scanner::new(ScanConfig {
        max_outstanding: 4,
        chunk_size: 512,
        ..ScanConfig::default()
    })?;
    let (_, outcome) = drive_to(&mut scanner, fixture.image(), ScanPhase::Done)?;
    outcome?;
    let output = match scanner.step()? {
        ScanStep::Done(output) => output,
        step => return Err(format!("expected Done, received {step:?}").into()),
    };
    let manifest = fixture.manifest();
    // Each base record appears once in the oracle sum, regardless of its hard-link paths.
    let unique: std::collections::BTreeMap<_, _> = manifest
        .entries
        .iter()
        .chain(&manifest.metafiles)
        .map(|entry| (entry.record, entry))
        .collect();
    let mut size = 0u64;
    let mut alloc = 0u64;
    let mut files = 0u64;
    for expected in unique.values() {
        for stream in &expected.streams {
            if stream.attribute_type == 0x80 && stream.name.is_empty() {
                size = size
                    .checked_add(stream.size)
                    .ok_or("manifest logical total")?;
            }
            if !stream.resident {
                alloc = alloc
                    .checked_add(
                        stream
                            .compressed_size
                            .or(stream.allocated_size)
                            .ok_or("manifest allocation")?,
                    )
                    .ok_or("manifest allocation total")?;
            }
        }
        if !matches!(expected.kind, adamite_fixtures::EntryKind::Directory)
            && expected.attributes & 0x10 == 0
        {
            files = files.checked_add(1).ok_or("manifest file count")?;
        }
    }
    for fill in &manifest.fill {
        size = size
            .checked_add(fill.total_size.ok_or("filler logical bytes")?)
            .ok_or("filler logical total")?;
        alloc = alloc
            .checked_add(fill.total_alloc.ok_or("filler allocation")?)
            .ok_or("filler allocation total")?;
        files = files
            .checked_add(u64::try_from(fill.count)?)
            .ok_or("filler file count")?;
    }
    let snapshot = &output.snapshot;
    let root = snapshot.node(crate::snapshot::NodeId::ROOT).ok_or("root")?;
    assert_eq!(
        (
            root.total_size(),
            root.total_alloc(),
            u64::from(root.file_count())
        ),
        (size, alloc, files),
        "{name}: independent root totals"
    );
    assert_eq!(
        output.stats.records_total,
        manifest
            .mft
            .data_size
            .checked_div(u64::from(manifest.geometry.file_record_size))
            .ok_or("record count")?
    );
    let indexed_files = snapshot.ext_stats().iter().try_fold(0u64, |count, group| {
        count
            .checked_add(group.count())
            .ok_or("extension file count")
    })?;
    assert_eq!(
        indexed_files, files,
        "{name}: every primary file enters one extension group"
    );
    assert!(snapshot.top_files().len() <= 100);
    if name == "basic-4k" {
        let links: Vec<_> = (0..u32::try_from(snapshot.len())?)
            .filter_map(|raw| snapshot.node(crate::snapshot::NodeId::from_raw(raw)))
            .filter(|node| {
                node.file_ref()
                    .is_some_and(|reference| reference.record() == 270)
            })
            .collect();
        assert_eq!(links.len(), 3);
        assert_eq!(
            links
                .iter()
                .filter(|node| !node
                    .flags()
                    .contains(crate::snapshot::NodeFlags::HARDLINK_DUP))
                .count(),
            1
        );
    }
    for _ in 0..3 {
        assert!(matches!(scanner.step(), Err(ScanError::Inactive)));
        assert!(matches!(
            scanner.feed(RequestId::from_raw(0), &[]),
            Err(FeedError::Inactive)
        ));
    }
    Ok(())
}

#[test]
fn aggregation_fixture_basic_4k_done_and_independent_totals() -> TestResult {
    aggregation_fixture("basic-4k")
}
#[test]
fn aggregation_fixture_frag_512_done_and_fill_totals() -> TestResult {
    aggregation_fixture("frag-512")
}
#[test]
fn aggregation_fixture_native_4kn_64k_done_and_independent_totals() -> TestResult {
    aggregation_fixture("4kn-64k")
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
    assert!(state.slots.get(6).ok_or("unallocated slot")?.base.is_none());
    assert!(
        state
            .slots
            .get(8)
            .ok_or("mismatched live slot")?
            .base
            .is_none()
    );
    assert_eq!(state.stats.deleted, 2);
    assert_eq!(
        state.stats.corrupt_records, 2,
        "only nonzero deleted mismatches and live mismatches are corrupt"
    );
    assert_eq!(state.stats.corrupt_headers, 2);
    Ok(())
}
