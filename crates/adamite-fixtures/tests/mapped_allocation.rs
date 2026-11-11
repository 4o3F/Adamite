//! Mapped allocation checked against committed image runlists and manifest headers.

use adamite_fixtures::{Fixture, MappedAllocation, ReferenceMft};
use adamite_ntfs::record::RecordClass;
use std::{collections::BTreeSet, error::Error, path::Path};

fn check_fixture(name: &str) -> Result<(), Box<dyn Error>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/ntfs")
        .join(format!("{name}.json"));
    let fixture = Fixture::load(&path)?;
    let reference = ReferenceMft::from_fixture(&fixture)?;
    let allocation = MappedAllocation::from_reference(&reference)?;
    let bad_name: Vec<_> = "$Bad".encode_utf16().collect();
    assert_eq!(
        allocation.stream_bytes(8, 0x80, &bad_name),
        Some(0),
        "{name}: $BadClus:$Bad maps no clusters"
    );

    let mut streams_with_holes = BTreeSet::new();
    let mut extension_streams = BTreeSet::new();
    for (ordinal, raw) in reference.records().enumerate() {
        let mut raw = raw.to_vec();
        let record = match RecordClass::classify(&mut raw) {
            RecordClass::Base(record) | RecordClass::Extension(record) => record,
            RecordClass::Unused | RecordClass::NotInUse(_) => continue,
            other => return Err(format!("{name}: unexpected record {ordinal}: {other:?}").into()),
        };
        let base = record.header().base_reference();
        let base_record = if base.is_null() {
            u64::try_from(ordinal)?
        } else {
            base.record()
        };
        for attribute in record.attributes() {
            let attribute = attribute?;
            let Some(descriptor) = attribute.non_resident() else {
                continue;
            };
            let key = (
                base_record,
                attribute.attribute_type().raw(),
                attribute.name().units().collect::<Vec<_>>(),
            );
            if !base.is_null() {
                extension_streams.insert(key.clone());
            }
            for run in descriptor.runs() {
                if run?.lcn().is_none() {
                    streams_with_holes.insert(key.clone());
                }
            }
        }
    }

    let mut ordinary_streams = 0;
    let mut checked_extensions = BTreeSet::new();
    let mut flagged_streams = 0;
    for entry in fixture
        .manifest()
        .entries
        .iter()
        .chain(&fixture.manifest().metafiles)
    {
        for stream in &entry.streams {
            let mapped = allocation.stream_bytes(entry.record, stream.attribute_type, &stream.name);
            if stream.resident {
                assert_eq!(mapped, None, "{name}: {} resident stream", entry.path);
                continue;
            }
            let mapped = mapped.ok_or_else(|| {
                format!("{name}: missing stream {}: {:?}", entry.path, stream.name)
            })?;
            let allocated = stream.allocated_size.ok_or("missing v3 allocated size")?;
            let key = (entry.record, stream.attribute_type, stream.name.clone());
            if stream.sparse || stream.compressed {
                flagged_streams += 1;
                // Compression padding and sparse holes occupy no clusters. The header's
                // physical size is only a consistency check; mapped runs define this reference.
                if stream.compressed_size == Some(mapped) {
                    eprintln!(
                        "{name}: {} {:?}: mapped bytes {mapped}, compressed_size {:?}",
                        entry.path, stream.name, stream.compressed_size
                    );
                }
                assert!(
                    mapped <= allocated,
                    "{name}: {} {:?}: mapped {mapped} exceeds allocated_size {allocated}",
                    entry.path,
                    stream.name
                );
            } else if !streams_with_holes.contains(&key) {
                ordinary_streams += 1;
                assert_eq!(
                    mapped, allocated,
                    "{name}: {} type {:#x} name {:?}",
                    entry.path, stream.attribute_type, stream.name
                );
                if extension_streams.contains(&key) {
                    assert!(entry.has_attribute_list, "{name}: {}", entry.path);
                    checked_extensions.insert(key);
                }
            }
        }
    }
    assert!(ordinary_streams > 0, "{name}: no ordinary streams checked");
    if name == "frag-512" {
        // Both MFT DATA and fragmented file DATA span extension records.
        assert!(checked_extensions.contains(&(0, 0x80, Vec::new())));
        assert!(checked_extensions.contains(&(10744, 0x80, Vec::new())));
    }
    if name == "basic-4k" {
        assert!(
            flagged_streams >= 2,
            "sparse and compressed streams checked"
        );
    }
    assert_eq!(allocation.stream_bytes(u64::MAX, 0x80, &[]), None);
    Ok(())
}

#[test]
fn basic_4k_mapped_allocation_matches_independent_headers() -> Result<(), Box<dyn Error>> {
    check_fixture("basic-4k")
}

#[test]
fn frag_512_mapped_allocation_joins_extension_segments() -> Result<(), Box<dyn Error>> {
    check_fixture("frag-512")
}

#[test]
fn native_4kn_64k_mapped_allocation_uses_volume_cluster_size() -> Result<(), Box<dyn Error>> {
    check_fixture("4kn-64k")
}
