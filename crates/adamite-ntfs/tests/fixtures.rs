#![allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]
//! Independent manifest oracles over the complete MFT of every committed image.

use adamite_fixtures::{EntryKind, Fixture, ReferenceMft};
use adamite_ntfs::{
    attribute::{AttributeFlags, AttributeType},
    record::{FileRecord, RecordClass, RecordFlags, RecordHeader, reference::FileRef},
    value::{
        file_name::FileName, index_allocation::IndexAllocation, reparse_point::ReparsePoint,
        standard_information::StandardInformation,
    },
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

type NameKey = (u64, u16, Vec<u16>, u8);
#[derive(Debug, Default)]
struct Stream {
    size: Option<u64>,
    allocated: Option<u64>,
    compressed_size: Option<u64>,
    resident: bool,
    sparse: bool,
    compressed: bool,
    runs: usize,
}
#[derive(Default)]
struct Observed {
    header: Option<RecordHeader>,
    standard: Option<StandardInformation>,
    names: BTreeSet<NameKey>,
    streams: BTreeMap<(u32, Vec<u16>), Stream>,
    reparse: Option<u32>,
    has_list: bool,
    has_index: bool,
}

fn observe(record: FileRecord<'_>, result: &mut Observed) {
    let header = record.header();
    if header.base_reference().is_null() {
        assert!(result.header.replace(header).is_none());
    }
    for attribute in record.attributes() {
        let attribute = attribute.expect("fixture attribute must parse");
        if attribute.attribute_type() == AttributeType::DATA || !attribute.is_resident() {
            let stream = result
                .streams
                .entry((
                    attribute.attribute_type().raw(),
                    attribute.name().units().collect(),
                ))
                .or_default();
            stream.resident = attribute.is_resident();
            stream.sparse = attribute.flags().contains(AttributeFlags::SPARSE);
            stream.compressed = attribute.flags().compression_format() != 0;
            if let Some(value) = attribute.resident_value() {
                assert!(stream.size.replace(value.len() as u64).is_none());
                stream.allocated = Some(0);
            } else {
                let descriptor = attribute.non_resident().unwrap();
                if let Some(sizes) = descriptor.sizes() {
                    assert!(stream.size.replace(sizes.data()).is_none());
                    assert!(stream.allocated.replace(sizes.allocated()).is_none());
                    stream.compressed_size = sizes.compressed();
                }
                for run in descriptor.runs() {
                    run.expect("fixture mapping pairs must parse");
                    stream.runs += 1;
                }
            }
        }

        match attribute.attribute_type() {
            AttributeType::STANDARD_INFORMATION => {
                result.standard = Some(StandardInformation::from_attribute(attribute).unwrap());
            }
            AttributeType::FILE_NAME => {
                let name = FileName::from_attribute(attribute).unwrap();
                result.names.insert((
                    name.parent().record(),
                    name.parent().sequence(),
                    name.name().units().collect(),
                    name.namespace(),
                ));
            }
            AttributeType::REPARSE_POINT => {
                result.reparse = Some(ReparsePoint::from_attribute(attribute).unwrap().tag())
            }
            AttributeType::ATTRIBUTE_LIST => result.has_list = true,
            AttributeType::INDEX_ALLOCATION => {
                let index = IndexAllocation::from_attribute(attribute).unwrap();
                for run in index.descriptor().runs() {
                    run.unwrap();
                }
                result.has_index = true;
            }
            _ => {}
        }
    }
}

fn assert_stream(actual: &Stream, expected: &adamite_fixtures::Stream, schema: u32, path: &str) {
    assert_eq!(
        (
            actual.size,
            actual.resident,
            actual.sparse,
            actual.compressed,
            actual.runs
        ),
        (
            Some(expected.size),
            expected.resident,
            expected.sparse,
            expected.compressed,
            expected.run_count
        ),
        "{path} stream {:?}",
        expected.name
    );
    if schema == 3 {
        assert_eq!(
            actual.allocated, expected.allocated_size,
            "{path} allocated bytes"
        );
        assert_eq!(
            actual.compressed_size, expected.compressed_size,
            "{path} compressed bytes"
        );
    }
}

fn verify(name: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("../../tests/fixtures/ntfs/{name}.json"));
    let fixture = Fixture::load(&path).unwrap_or_else(|error| panic!("{name}: {error}"));
    let mft =
        ReferenceMft::from_fixture(&fixture).unwrap_or_else(|error| panic!("{name}: {error}"));
    assert_eq!(mft.extents(), fixture.manifest().mft.extents);
    let mut observed: BTreeMap<u64, Observed> = BTreeMap::new();
    let mut extensions = Vec::new();
    for (number, bytes) in mft.records().enumerate() {
        let mut raw = bytes.to_vec();
        match RecordClass::classify(&mut raw) {
            RecordClass::Base(record) => {
                observe(record, observed.entry(number as u64).or_default())
            }
            RecordClass::Extension(record) => {
                let base = record.header().base_reference();
                extensions.push(base);
                observe(record, observed.entry(base.record()).or_default());
            }
            RecordClass::Corrupt(error) => panic!("{name} record {number}: {error}"),
            _ => {}
        }
    }
    for base in extensions {
        assert_eq!(
            observed[&base.record()].header.unwrap().sequence(),
            base.sequence()
        );
    }
    for entry in fixture
        .manifest()
        .entries
        .iter()
        .chain(&fixture.manifest().metafiles)
    {
        let actual = observed
            .get(&entry.record)
            .unwrap_or_else(|| panic!("missing {}", entry.path));
        let header = actual.header.expect("manifest entry must be a Base record");
        assert_eq!(header.sequence(), entry.seq, "{} sequence", entry.path);
        assert_eq!(
            u64::from(header.hard_links()),
            entry.nlink,
            "{} hard links",
            entry.path
        );
        assert_eq!(
            header.flags().contains(RecordFlags::DIRECTORY),
            matches!(entry.kind, EntryKind::Directory),
            "{} kind",
            entry.path
        );
        assert_eq!(
            matches!(entry.kind, EntryKind::Symlink),
            actual.reparse == Some(0xa000000c),
            "{} symlink",
            entry.path
        );
        let standard = actual
            .standard
            .expect("manifest entry must have STANDARD_INFORMATION");
        assert_eq!(
            standard.file_attributes().raw(),
            entry.attributes,
            "{} flags",
            entry.path
        );
        assert_eq!(
            standard.times().modified(),
            entry.mtime_filetime,
            "{} mtime",
            entry.path
        );
        let names: BTreeSet<_> = entry
            .names
            .iter()
            .map(|name| {
                (
                    name.parent_record,
                    name.parent_seq,
                    name.name.clone(),
                    name.namespace,
                )
            })
            .collect();
        assert_eq!(actual.names, names, "{} names", entry.path);
        assert_eq!(
            actual.reparse, entry.reparse_tag,
            "{} reparse tag",
            entry.path
        );
        assert_eq!(
            actual.has_list, entry.has_attribute_list,
            "{} attribute list",
            entry.path
        );
        assert_eq!(
            actual
                .streams
                .keys()
                .filter(|(kind, _)| fixture.manifest().schema_version == 3 || *kind == 0x80)
                .count(),
            entry.streams.len(),
            "{} stream count",
            entry.path
        );
        for expected in &entry.streams {
            let stream = &actual.streams[&(expected.attribute_type, expected.name.clone())];
            assert_stream(
                stream,
                expected,
                fixture.manifest().schema_version,
                &entry.path,
            );
        }
        if entry.path == "/bigdir" {
            assert!(actual.has_index);
        }
    }
    for fill in &fixture.manifest().fill {
        let prefix: Vec<_> = fill.name_prefix.encode_utf16().collect();
        let parent = FileRef::new(fill.parent_record, fill.parent_seq).unwrap();
        let mut count = 0;
        let mut totals = (0, 0, 0);
        for (&number, actual) in &observed {
            let belongs = actual
                .names
                .iter()
                .filter(|name| name.0 == parent.record())
                .collect::<Vec<_>>();
            if belongs.is_empty() {
                continue;
            }
            assert!(
                (fill.record_min..=fill.record_max).contains(&number),
                "{} record range",
                fill.dir
            );
            let mut raw = mft.record(number).unwrap().to_vec();
            assert!(
                matches!(RecordClass::classify(&mut raw), RecordClass::Base(_)),
                "{} record {number} is not Base",
                fill.dir
            );
            for name in belongs {
                assert_eq!(name.1, parent.sequence());
                assert_eq!(name.2.len(), fill.name_len);
                assert!(name.2.starts_with(&prefix));
            }
            count += 1;
            if fixture.manifest().schema_version == 3 {
                for ((kind, name), stream) in &actual.streams {
                    if *kind == 0x80 {
                        if name.is_empty() {
                            totals.0 += stream.size.unwrap();
                        } else {
                            totals.1 += stream.size.unwrap();
                        }
                    }
                    totals.2 += stream.compressed_size.unwrap_or(stream.allocated.unwrap());
                }
            }
        }
        assert_eq!(count, fill.count, "{} filler count", fill.dir);
        if fixture.manifest().schema_version == 3 {
            assert_eq!(
                (Some(totals.0), Some(totals.1), Some(totals.2)),
                (fill.total_size, fill.total_ads_size, fill.total_alloc),
                "{} filler totals",
                fill.dir
            );
        }
    }
    let root = &observed[&5];
    assert!(root.names.iter().any(|name| name.0 == 5 && name.1 == 5));
    assert_eq!(
        observed[&0].has_list,
        fixture.manifest().mft.has_attribute_list
    );
    for deleted in &fixture.manifest().deleted {
        let mut raw = mft.record(deleted.record).unwrap().to_vec();
        assert!(
            matches!(RecordClass::classify(&mut raw), RecordClass::NotInUse(_)),
            "{} deleted record",
            deleted.path
        );
    }
}

#[test]
fn basic_4k_matches_every_manifest_oracle() {
    verify("basic-4k");
}
#[test]
fn fragmented_mft_streams_and_filler_sets_match_the_manifest() {
    verify("frag-512");
}
#[test]
fn native_4kn_geometry_and_records_match_the_manifest() {
    verify("4kn-64k");
}

#[test]
fn v3_allocations_use_first_segment_values_for_all_stream_types() {
    use adamite_fixtures::builders::{AttrBuilder, NonResidentModel, RecordBuilder};
    for (kind, flags, resident, size, allocated, physical) in [
        (0x80, 0, true, 5, 0, None),
        (0x80, 0, false, 5000, 8192, None),
        (0x80, 0x8000, false, 1048576, 1048576, Some(8192)),
        (0x80, 1, false, 262144, 262144, Some(32768)),
        (0xa0, 0, false, 4096, 8192, None),
        (0xb0, 0, false, 12, 4096, None),
    ] {
        let mut attribute = if resident {
            AttrBuilder::resident(kind, vec![0; size as usize])
        } else {
            AttrBuilder::non_resident(
                kind,
                NonResidentModel {
                    sizes: [allocated, size, size, physical.unwrap_or(0)],
                    ..Default::default()
                },
            )
        };
        attribute.flags = flags;
        let mut bytes = RecordBuilder {
            attributes: vec![attribute.build()],
            ..Default::default()
        }
        .build();
        let RecordClass::Base(record) = RecordClass::classify(&mut bytes) else {
            panic!("synthetic record must be a base")
        };
        let mut actual = Observed::default();
        observe(record, &mut actual);
        let expected: adamite_fixtures::Stream=serde_json::from_value(serde_json::json!({
            "attribute_type":kind,"name":[],"size":size.to_string(),"allocated_size":allocated.to_string(),
            "compressed_size":physical.map(|v|v.to_string()),"resident":resident,"sparse":flags==0x8000,
            "compressed":flags==1,"run_count":if resident {0} else {1}
        })).unwrap();
        assert_stream(
            &actual.streams[&(kind, Vec::new())],
            &expected,
            3,
            "synthetic",
        );
    }
}
