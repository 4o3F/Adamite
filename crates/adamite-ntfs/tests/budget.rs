#![allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]
//! Compressed fixture budget and compiled reference bootstrap support.
mod support;
use std::{fs, io, path::Path};
use support::builders::{
    AttrBuilder, AttrListBuilder, AttrListEntryModel, BootSectorBuilder, NonResidentModel,
    RecordBuilder,
};
use support::fixture::{Fixture, Manifest};

fn compressed_size(directory: &Path) -> io::Result<u64> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    let mut total = 0u64;
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_file() && entry.file_name().to_string_lossy().ends_with(".img.zst")
        {
            total = total
                .checked_add(entry.metadata()?.len())
                .ok_or_else(|| io::Error::other("fixture size overflow"))?;
        }
    }
    Ok(total)
}
#[test]
fn compressed_fixtures_fit_one_mib_including_an_empty_directory() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/ntfs");
    let size = compressed_size(&root).unwrap();
    assert!(
        size <= 1024 * 1024,
        "compressed fixture bytes {size} exceed 1 MiB"
    );
}

fn manifest_value() -> serde_json::Value {
    serde_json::json!({ "schema_version": 2, "image": "synthetic.img.zst", "image_len": "65536", "generator": {},
      "geometry": { "bytes_per_sector": 512, "cluster_size": 512, "file_record_size": 1024, "index_record_size": 4096, "total_clusters": "128", "mft_lcn": "4", "mftmirr_lcn": "8" },
      "mft": { "extents": [{"vcn":"0","lcn":"4","len":"4"},{"vcn":"4","lcn":"12","len":"12"}], "data_size":"8192", "has_attribute_list":true }, "entries":[], "fill":[], "deleted":[] })
}

#[test]
fn every_manifest_fits_256_kib() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/ntfs");
    for entry in fs::read_dir(root).unwrap() {
        let entry = entry.unwrap();
        if entry
            .path()
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            assert!(
                entry.metadata().unwrap().len() <= 256 * 1024,
                "{} exceeds 256 KiB",
                entry.path().display()
            );
        }
    }
}
fn manifest() -> Manifest {
    serde_json::from_value(manifest_value()).unwrap()
}
#[test]
fn manifest_rejects_unknown_fields_and_preserves_u64_strings() {
    let mut value = manifest_value();
    value["mft"]["data_size"] = serde_json::json!(u64::MAX.to_string());
    assert_eq!(
        serde_json::from_value::<Manifest>(value)
            .unwrap()
            .mft
            .data_size,
        u64::MAX
    );
    let mut value = manifest_value();
    value["unexpected"] = serde_json::json!(true);
    assert!(serde_json::from_value::<Manifest>(value).is_err());
    let mut value = manifest_value();
    value["geometry"]["unexpected"] = serde_json::json!(true);
    assert!(serde_json::from_value::<Manifest>(value).is_err());
    let mut value = manifest_value();
    value["fill"] = serde_json::json!([{"dir":"/fill","count":2,"name_prefix":"f","name_len":255,"record_min":"67","record_max":"69","parent_record":"64","parent_seq":1}]);
    assert_eq!(
        serde_json::from_value::<Manifest>(value.clone())
            .unwrap()
            .fill[0]
            .record_max,
        69
    );
    value["fill"][0]["unexpected"] = serde_json::json!(true);
    assert!(serde_json::from_value::<Manifest>(value).is_err());
}
#[test]
fn bootstrap_follows_an_attribute_list_only_through_known_extents() {
    let list = AttrListBuilder {
        entries: vec![
            AttrListEntryModel {
                lowest: 0,
                segment: 7u64 << 48,
                id: 0,
                ..Default::default()
            },
            AttrListEntryModel {
                lowest: 4,
                segment: (7u64 << 48) | 1,
                id: 9,
                ..Default::default()
            },
        ],
        zero_tail: 0,
    }
    .build();
    let first = AttrBuilder::non_resident(
        0x80,
        NonResidentModel {
            highest: 3,
            sizes: [8192; 4],
            mapping_pairs: vec![0x11, 4, 4, 0],
            ..Default::default()
        },
    )
    .build();
    let record0 = RecordBuilder {
        record_number: 0,
        attributes: vec![first, AttrBuilder::resident(0x20, list).build()],
        ..Default::default()
    }
    .build();
    let mut continuation = AttrBuilder::non_resident(
        0x80,
        NonResidentModel {
            lowest: 4,
            highest: 15,
            sizes: [-1; 4],
            mapping_pairs: vec![0x11, 12, 12, 0],
            ..Default::default()
        },
    );
    continuation.id = 9;
    let record1 = RecordBuilder {
        record_number: 1,
        base: 7u64 << 48,
        attributes: vec![continuation.build()],
        ..Default::default()
    }
    .build();
    let mut mft = vec![0; 8192];
    mft[..1024].copy_from_slice(&record0);
    mft[1024..2048].copy_from_slice(&record1);
    mft[2048..3072].copy_from_slice(
        &RecordBuilder {
            record_number: 2,
            ..Default::default()
        }
        .build(),
    );
    let mut image = vec![0; 65536];
    image[..512].copy_from_slice(
        &BootSectorBuilder {
            cluster_encoding: 1,
            file_encoding: 2,
            sectors: 128,
            mft: 4,
            mirror: 8,
            ..Default::default()
        }
        .build(),
    );
    image[2048..4096].copy_from_slice(&mft[..2048]);
    image[6144..12288].copy_from_slice(&mft[2048..]);
    let fixture = Fixture::from_image(image.clone(), manifest()).unwrap();
    assert_eq!(fixture.records().len(), 8);
    assert_eq!(fixture.record(2).unwrap(), &mft[2048..3072]);
    assert!(fixture.record(8).is_none());
    let mut unreachable = image;
    unreachable[2048 + 56 + 64 + 2] = 1;
    assert!(Fixture::from_image(unreachable, manifest()).is_err());
}
