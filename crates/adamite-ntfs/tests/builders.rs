#![allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

//! Independent encoders produce the requested on-disk byte layouts.

mod support;

use support::builders::{AttrBuilder, BootSectorBuilder, NonResidentModel, RecordBuilder};
use support::runlist_encoder::{RunModel, RunlistEncoder};

#[test]
fn boot_builder_encodes_literals_and_exponents() {
    for (cluster, file, index) in [(1, 2, -12), (0xf4, -10, -16), (0xf8, -9, -12)] {
        let boot = BootSectorBuilder {
            cluster_encoding: cluster,
            file_encoding: file,
            index_encoding: index,
            ..Default::default()
        }
        .build();
        assert_eq!(&boot[3..11], b"NTFS    ");
        assert_eq!(boot[0x0d], cluster);
        assert_eq!(boot[0x40], file as u8);
        assert_eq!(boot[0x44], index as u8);
        assert_eq!(&boot[510..], &[0x55, 0xaa]);
    }
}

#[test]
fn attribute_builder_encodes_storage_names_and_extended_fields() {
    let mut resident = AttrBuilder::resident(0x80, vec![1, 2, 3]);
    resident.name = vec![0xd800, 0, 0x61];
    resident.id = 9;
    resident.flags = 0x4000;
    resident.body_gap = 1;
    let bytes = resident.build();
    assert_eq!(bytes[8], 0);
    assert_eq!(bytes[9], 3);
    assert_eq!(&bytes[24..30], &[0, 0xd8, 0, 0, 0x61, 0]);
    assert_eq!(&bytes[31..34], &[1, 2, 3]);
    assert_eq!(u16::from_le_bytes(bytes[20..22].try_into().unwrap()), 31);
    let mut nonresident = AttrBuilder::non_resident(
        0x80,
        NonResidentModel {
            compression_unit: 4,
            sizes: [1, 2, 3, 4],
            ..Default::default()
        },
    );
    nonresident.flags = 0x8000;
    let bytes = nonresident.build();
    assert_eq!(bytes[8], 1);
    assert_eq!(bytes[34], 4);
    assert_eq!(u16::from_le_bytes(bytes[32..34].try_into().unwrap()), 72);
    assert_eq!(i64::from_le_bytes(bytes[64..72].try_into().unwrap()), 4);
    assert_eq!(&bytes[72..76], &[0x11, 1, 1, 0]);
    assert_eq!(bytes.len() % 8, 0);
}

#[test]
fn runlist_encoder_preserves_sparse_bases_signed_deltas_and_widths() {
    let encoder = RunlistEncoder {
        runs: vec![
            RunModel {
                len: 5,
                lcn: Some(100),
            },
            RunModel { len: 16, lcn: None },
            RunModel {
                len: 3,
                lcn: Some(90),
            },
        ],
    };
    assert_eq!(
        encoder.encode(0),
        [0x11, 5, 100, 0x01, 16, 0x11, 3, 0xf6, 0]
    );
    let mut padded = vec![0x88];
    padded.extend(5u64.to_le_bytes());
    padded.extend(100i64.to_le_bytes());
    padded.push(0x08);
    padded.extend(16u64.to_le_bytes());
    padded.push(0x88);
    padded.extend(3u64.to_le_bytes());
    padded.extend((-10i64).to_le_bytes());
    padded.push(0);
    assert_eq!(encoder.encode(8), padded);
    assert_eq!(RunlistEncoder { runs: vec![] }.encode(0), [0]);
}

#[test]
fn record_builder_layout_attributes_and_every_protected_tail() {
    for size in [512, 1024, 4096, 65536] {
        for usa_offset in [0x2a, 0x30] {
            let builder = RecordBuilder {
                size,
                usa_offset,
                usn: 0xffff,
                attributes: vec![vec![0x24; 24]],
                ..Default::default()
            };
            let plain = builder.plain();
            let protected = builder.protect(&plain);
            let first = builder.first_attribute();
            assert_eq!(&plain[first..first + 24], &[0x24; 24]);
            assert_eq!(&plain[first + 24..first + 28], &u32::MAX.to_le_bytes());
            for stride in 0..size / 512 {
                let tail = (stride + 1) * 512 - 2;
                let entry = usize::from(usa_offset) + (stride + 1) * 2;
                assert_eq!(&protected[tail..tail + 2], &[0xff, 0xff]);
                assert_eq!(&protected[entry..entry + 2], &plain[tail..tail + 2]);
                let mut torn = protected.clone();
                builder.corrupt_tail(&mut torn, stride);
                assert_eq!(
                    torn.iter().zip(&protected).filter(|(a, b)| a != b).count(),
                    1
                );
            }
        }
    }
}
