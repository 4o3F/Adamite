#![allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

//! Boot-sector scenarios, independent golden vectors and geometry properties.

mod support;

use adamite_fixtures::builders::BootSectorBuilder;
use adamite_ntfs::{GeometryError, parse_boot_sector};
use proptest::prelude::*;

fn cluster_model(sector: u16, encoding: u8) -> Option<u128> {
    let sectors = match encoding {
        1 | 2 | 4 | 8 | 16 | 32 | 64 | 128 => u128::from(encoding),
        0xf4..=0xff => 2u128.pow(256 - u32::from(encoding)),
        _ => return None,
    };
    let size = sectors * u128::from(sector);
    (size <= 2_097_152).then_some(size)
}

fn record_model(cluster: u128, encoding: i8) -> Option<u128> {
    let size = if encoding > 0 {
        cluster * u128::try_from(encoding).unwrap()
    } else if encoding < 0 {
        2u128.checked_pow(u32::from(encoding.unsigned_abs()))?
    } else {
        return None;
    };
    (size.is_power_of_two() && (512..=65536).contains(&size)).then_some(size)
}

fn valid_geometry() -> impl Strategy<Value = BootSectorBuilder> {
    let mut pairs = Vec::new();
    for sector in [512, 1024, 2048, 4096] {
        for encoding in 0..=255 {
            if cluster_model(sector, encoding).is_some() {
                pairs.push((sector, encoding));
            }
        }
    }
    prop::sample::select(pairs)
        .prop_flat_map(|(sector, encoding)| {
            let cluster = cluster_model(sector, encoding).unwrap();
            let records: Vec<_> = (i8::MIN..=i8::MAX)
                .filter(|value| record_model(cluster, *value).is_some())
                .collect();
            (
                Just(sector),
                Just(encoding),
                prop::sample::select(records.clone()),
                prop::sample::select(records),
                (1u64 << 24)..(1u64 << 30),
                0u64..64,
                0u64..64,
                any::<u64>(),
            )
        })
        .prop_map(
            |(
                sector,
                cluster_encoding,
                file_encoding,
                index_encoding,
                sectors,
                mft,
                mirror,
                serial,
            )| BootSectorBuilder {
                sector,
                cluster_encoding,
                file_encoding,
                index_encoding,
                sectors,
                mft,
                mirror,
                serial,
            },
        )
}

proptest! {
    #![proptest_config(support::proptest_config("proptest-regressions/geometry.txt"))]
    #[test]
    fn valid_geometry_round_trips(model in valid_geometry()) {
        let geometry = parse_boot_sector(&model.build()).unwrap();
        let cluster = cluster_model(model.sector, model.cluster_encoding).unwrap();
        prop_assert_eq!(geometry.bytes_per_sector(), model.sector);
        prop_assert_eq!(u128::from(geometry.cluster_size()), cluster);
        prop_assert_eq!(u128::from(geometry.sectors_per_cluster()), cluster / u128::from(model.sector));
        prop_assert_eq!(u128::from(geometry.file_record_size()), record_model(cluster, model.file_encoding).unwrap());
        prop_assert_eq!(u128::from(geometry.index_record_size()), record_model(cluster, model.index_encoding).unwrap());
        prop_assert_eq!(geometry.total_sectors(), model.sectors);
        prop_assert_eq!(u128::from(geometry.total_bytes()), u128::from(model.sectors) * u128::from(model.sector));
        prop_assert_eq!(u128::from(geometry.total_clusters()), u128::from(model.sectors) / (cluster / u128::from(model.sector)));
        prop_assert_eq!(geometry.mft_lcn(), model.mft);
        prop_assert_eq!(geometry.mftmirr_lcn(), model.mirror);
        prop_assert_eq!(geometry.serial(), model.serial);
    }

    #[test]
    fn suffix_and_ignored_bytes_do_not_change_geometry(model in valid_geometry(), suffix in prop::collection::vec(any::<u8>(), 0..=256), ignored in prop::collection::vec(any::<u8>(), 512)) {
        let boot = model.build();
        let expected = parse_boot_sector(&boot).unwrap();
        let mut changed = boot.to_vec();
        for index in 0..512 {
            if !matches!(index, 3..=13 | 0x28..=0x40 | 0x44 | 0x48..=0x4f | 510..=511) {
                changed[index] = ignored[index];
            }
        }
        changed.extend(suffix);
        prop_assert_eq!(parse_boot_sector(&changed), Ok(expected));
    }

    #[test]
    fn cluster_ranges_match_u128_model(model in valid_geometry(), random_lcn in any::<u64>(), random_count in any::<u64>()) {
        let geometry = parse_boot_sector(&model.build()).unwrap();
        let total = geometry.total_clusters();
        for lcn in [0, 1, total - 1, total, total + 1, u64::MAX, random_lcn] {
            for count in [0, 1, total - 1, total, total + 1, u64::MAX, random_count] {
                let result = geometry.cluster_byte_range(lcn, count);
                let end = u128::from(lcn) + u128::from(count);
                let valid = count > 0 && end <= u128::from(total);
                prop_assert_eq!(result.is_ok(), valid, "lcn={}, count={}, total={}", lcn, count, total);
                if let Ok(range) = result {
                    prop_assert_eq!(u128::from(range.start), u128::from(lcn) * u128::from(geometry.cluster_size()));
                    prop_assert_eq!(u128::from(range.end), end * u128::from(geometry.cluster_size()));
                }
            }
        }
    }

    #[test]
    fn arbitrary_boot_bytes_are_total(bytes in prop::collection::vec(any::<u8>(), 0..=65536)) {
        let _ = parse_boot_sector(&bytes);
    }
}

#[test]
fn documented_windows_and_real_mkntfs_golden_sectors() {
    for (bytes, sectors, mirror, serial) in [
        (
            &include_bytes!("golden/windows-2000-boot.bin")[..],
            0x7ff54a,
            0x7ff54,
            0x1c741bc9741ba514,
        ),
        (
            &include_bytes!("golden/mkntfs-boot.bin")[..],
            32767,
            2047,
            3816218020381368311,
        ),
    ] {
        let geometry = parse_boot_sector(bytes).unwrap();
        assert_eq!(geometry.bytes_per_sector(), 512);
        assert_eq!(geometry.sectors_per_cluster(), 8);
        assert_eq!(geometry.cluster_size(), 4096);
        assert_eq!(geometry.file_record_size(), 1024);
        assert_eq!(geometry.index_record_size(), 4096);
        assert_eq!(geometry.total_sectors(), sectors);
        assert_eq!(geometry.mft_lcn(), 4);
        assert_eq!(geometry.mftmirr_lcn(), mirror);
        assert_eq!(geometry.serial(), serial);
    }
}

#[test]
fn every_short_input_is_truncated_and_every_identification_byte_is_checked() {
    let boot = BootSectorBuilder::default().build();
    for length in 0..512 {
        assert_eq!(
            parse_boot_sector(&boot[..length]),
            Err(GeometryError::Truncated)
        );
    }
    for offset in (3..11).chain([510, 511]) {
        let mut changed = boot;
        changed[offset] ^= 1;
        assert_eq!(
            parse_boot_sector(&changed),
            Err(GeometryError::Identification)
        );
    }
}

#[test]
fn exhaustive_sector_cluster_and_record_encodings() {
    for sector in [512, 1024, 2048, 4096] {
        for encoding in 0..=255 {
            let model = BootSectorBuilder {
                sector,
                cluster_encoding: encoding,
                ..Default::default()
            };
            let result = parse_boot_sector(&model.build());
            assert_eq!(
                result.is_ok(),
                cluster_model(sector, encoding).is_some(),
                "sector={sector}, cluster={encoding:#x}"
            );
            if let Ok(geometry) = result {
                for raw in 0..=255u8 {
                    let record = raw as i8;
                    for file in [true, false] {
                        let mut changed = model.clone();
                        if file {
                            changed.file_encoding = record;
                        } else {
                            changed.index_encoding = record;
                        }
                        let valid =
                            record_model(u128::from(geometry.cluster_size()), record).is_some();
                        let parsed = parse_boot_sector(&changed.build());
                        assert_eq!(
                            parsed.is_ok(),
                            valid,
                            "sector={sector}, cluster={encoding:#x}, record={record}, file={file}"
                        );
                        if !valid {
                            let expected = if file {
                                GeometryError::InvalidFileRecordSize { encoding: record }
                            } else {
                                GeometryError::InvalidIndexRecordSize { encoding: record }
                            };
                            assert_eq!(parsed, Err(expected));
                        }
                    }
                }
            }
        }
    }
    for sector in [0, 1, 511, 513, 8192, u16::MAX] {
        assert_eq!(
            parse_boot_sector(
                &BootSectorBuilder {
                    sector,
                    ..Default::default()
                }
                .build()
            ),
            Err(GeometryError::InvalidSectorSize { size: sector })
        );
    }
}

#[test]
fn exponent_golden_cases_and_positive_record_encoding() {
    for (encoding, size) in [(0xf4, 2_097_152), (0xf8, 131_072)] {
        let boot = BootSectorBuilder {
            cluster_encoding: encoding,
            ..Default::default()
        }
        .build();
        assert_eq!(parse_boot_sector(&boot).unwrap().cluster_size(), size);
    }
    let oversized = BootSectorBuilder {
        sector: 4096,
        cluster_encoding: 0xf4,
        ..Default::default()
    };
    assert_eq!(
        parse_boot_sector(&oversized.build()),
        Err(GeometryError::ClusterTooLarge { size: 16_777_216 })
    );
    let positive = BootSectorBuilder {
        cluster_encoding: 1,
        file_encoding: 2,
        ..Default::default()
    };
    assert_eq!(
        parse_boot_sector(&positive.build())
            .unwrap()
            .file_record_size(),
        1024
    );
}

#[test]
fn sector_counts_and_mft_locations_are_bounded_by_complete_clusters() {
    for sectors in [0, 1, 7, 1 << 63, u64::MAX, u64::MAX / 512 + 1] {
        assert_eq!(
            parse_boot_sector(
                &BootSectorBuilder {
                    sectors,
                    ..Default::default()
                }
                .build()
            ),
            Err(GeometryError::InvalidTotalSectors { sectors })
        );
    }
    let max_sectors = u64::MAX / 512;
    let maximum = BootSectorBuilder {
        sectors: max_sectors,
        ..Default::default()
    };
    assert!(parse_boot_sector(&maximum.build()).is_ok());
    let base = BootSectorBuilder {
        cluster_encoding: 1,
        sectors: 17,
        mft: 15,
        mirror: 15,
        ..Default::default()
    };
    assert!(parse_boot_sector(&base.build()).is_ok());
    for lcn in [16, 17, 18, u64::MAX] {
        let mut changed = base.clone();
        changed.mft = lcn;
        assert_eq!(
            parse_boot_sector(&changed.build()),
            Err(GeometryError::MftOutOfVolume { lcn })
        );
        let mut changed = base.clone();
        changed.mirror = lcn;
        assert_eq!(
            parse_boot_sector(&changed.build()),
            Err(GeometryError::MftMirrorOutOfVolume { lcn })
        );
    }
    let partial = BootSectorBuilder {
        sectors: 15,
        mft: 0,
        mirror: 0,
        ..Default::default()
    };
    let geometry = parse_boot_sector(&partial.build()).unwrap();
    assert_eq!(geometry.total_clusters(), 1);
    assert_eq!(geometry.total_bytes(), 15 * 512);
    assert_eq!(geometry.cluster_byte_range(0, 1).unwrap(), 0..4096);
    assert!(geometry.cluster_byte_range(1, 1).is_err());
}

#[test]
fn validation_order_is_stable() {
    let mut boot = BootSectorBuilder {
        sector: 0,
        sectors: 0,
        ..Default::default()
    }
    .build();
    boot[3] = 0;
    assert_eq!(parse_boot_sector(&boot), Err(GeometryError::Identification));
    boot[3] = b'N';
    assert_eq!(
        parse_boot_sector(&boot),
        Err(GeometryError::InvalidSectorSize { size: 0 })
    );
}
