//! Bootstrap phase regressions.

use super::support::{DriveOutcome, TestResult, boot, data, drive_to, image, record};
use crate::error::ScanError;
use crate::mft::bootstrap::{ListStorage, Record0, RecordParts};
use crate::mft::mapping::OwnedRun;
use crate::scan::{ScanConfig, ScanPhase, Scanner};
use adamite_fixtures::Fixture;
use adamite_fixtures::builders::{
    AttrBuilder, AttrListBuilder, AttrListEntryModel, BootSectorBuilder, NonResidentModel,
    RecordBuilder,
};
use adamite_ntfs::attribute::AttributeType;
use adamite_ntfs::boot::VolumeGeometry;
use adamite_ntfs::record::fixup::FixupError;
use adamite_ntfs::record::reference::FileRef;
use adamite_ntfs::record::{FileRecord, RecordError};
use std::error::Error;
use std::path::Path;

/// Hold requests until the driver reports waiting, then respond in reverse order.
fn drive(scanner: &mut Scanner, image: &[u8]) -> Result<DriveOutcome, Box<dyn Error>> {
    drive_to(scanner, image, ScanPhase::Discover)
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
