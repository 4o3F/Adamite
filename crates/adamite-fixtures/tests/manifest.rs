//! Strict versioned schema checks over a small synthetic v3 manifest.

use adamite_fixtures::{Fixture, Manifest};
use serde_json::{Value, json};

fn v3() -> Value {
    serde_json::from_str(include_str!("data/v3.json")).unwrap()
}

#[test]
fn v2_defaults_keep_data_types_and_missing_measurements_absent() {
    let mut legacy = v3();
    legacy["schema_version"] = json!(2);
    legacy.as_object_mut().unwrap().remove("metafiles");
    for entry in legacy["entries"].as_array_mut().unwrap() {
        let streams = entry["streams"].as_array_mut().unwrap();
        streams.retain(|stream| stream["attribute_type"] == 0x80);
        for stream in streams {
            let fields = stream.as_object_mut().unwrap();
            for field in ["attribute_type", "allocated_size", "compressed_size"] {
                fields.remove(field);
            }
        }
    }
    for fill in legacy["fill"].as_array_mut().unwrap() {
        let fields = fill.as_object_mut().unwrap();
        for field in ["total_size", "total_ads_size", "total_alloc"] {
            fields.remove(field);
        }
    }
    let manifest: Manifest = serde_json::from_value(legacy).unwrap();
    assert!(manifest.metafiles.is_empty());
    for entry in &manifest.entries {
        for stream in &entry.streams {
            assert_eq!(stream.attribute_type, 0x80);
            assert_eq!(stream.allocated_size, None);
            assert_eq!(stream.compressed_size, None);
        }
    }
    for fill in &manifest.fill {
        assert_eq!(
            (fill.total_size, fill.total_ads_size, fill.total_alloc),
            (None, None, None)
        );
    }
}

#[test]
fn v3_preserves_exact_sizes_types_metafiles_and_filler_totals() {
    let manifest: Manifest = serde_json::from_value(v3()).unwrap();
    assert_eq!(manifest.schema_version, 3);
    assert_eq!(manifest.entries[0].streams[0].allocated_size, Some(0));
    assert_eq!(manifest.entries[0].streams[0].compressed_size, None);
    assert_eq!(manifest.entries[0].streams[1].compressed_size, Some(8192));
    assert_eq!(manifest.entries[0].streams[2].attribute_type, 0xa0);
    assert_eq!(
        manifest.entries[0].streams[2].allocated_size,
        Some(u64::MAX)
    );
    assert_eq!(manifest.metafiles[0].record, 0);
    assert_eq!(manifest.fill[0].total_alloc, Some(u64::MAX));
}

#[test]
fn v3_requires_every_stream_measurement_in_entries_and_metafiles() {
    for collection in ["entries", "metafiles"] {
        for field in ["attribute_type", "allocated_size", "compressed_size"] {
            let mut value = v3();
            value[collection][0]["streams"][0]
                .as_object_mut()
                .unwrap()
                .remove(field);
            assert!(
                serde_json::from_value::<Manifest>(value).is_err(),
                "{collection}.{field}"
            );
        }
    }
}

#[test]
fn v3_requires_metafiles_filler_totals_and_supported_schema() {
    for field in ["total_size", "total_ads_size", "total_alloc"] {
        let mut value = v3();
        value["fill"][0].as_object_mut().unwrap().remove(field);
        assert!(
            serde_json::from_value::<Manifest>(value).is_err(),
            "{field}"
        );
    }
    let mut value = v3();
    value.as_object_mut().unwrap().remove("metafiles");
    assert!(serde_json::from_value::<Manifest>(value).is_err());
    for version in [0, 1, 4] {
        let mut value = v3();
        value["schema_version"] = json!(version);
        assert!(
            serde_json::from_value::<Manifest>(value).is_err(),
            "version {version}"
        );
    }
}

#[test]
fn v3_rejects_unknown_fields_wrong_types_and_invalid_storage_measurements() {
    for (field, value) in [
        ("unknown", json!(0)),
        ("attribute_type", json!(0)),
        ("allocated_size", json!(12)),
        ("allocated_size", json!("-1")),
        ("allocated_size", Value::Null),
        ("allocated_size", json!("1")),
        ("compressed_size", json!(false)),
        ("compressed_size", json!("1")),
        ("attribute_type", json!(160)),
    ] {
        let mut input = v3();
        input["entries"][0]["streams"][0][field] = value;
        assert!(
            serde_json::from_value::<Manifest>(input).is_err(),
            "{field}"
        );
    }
    let mut input = v3();
    input["entries"][0]["streams"][1]["compressed_size"] = Value::Null;
    assert!(serde_json::from_value::<Manifest>(input).is_err());
    let mut input = v3();
    input["unexpected"] = json!(true);
    assert!(serde_json::from_value::<Manifest>(input).is_err());
    let mut input = v3();
    input["fill"][0]["total_size"] = json!(1024);
    assert!(serde_json::from_value::<Manifest>(input).is_err());
}

#[test]
fn from_image_rechecks_programmatically_changed_v3_measurements() {
    let mut manifest: Manifest = serde_json::from_value(v3()).unwrap();
    manifest.image_len = 512;
    manifest.entries[0].streams[0].allocated_size = None;
    assert!(Fixture::from_image(vec![0; 512], manifest).is_err());
}
