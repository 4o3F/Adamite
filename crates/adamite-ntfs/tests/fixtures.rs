#![allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]
//! Independent manifest oracles over the complete MFT of every committed image.
mod support;

use adamite_ntfs::{
    AttributeFlags, AttributeType, FileName, FileRecord, FileRef, IndexAllocation, RecordClass,
    RecordFlags, RecordHeader, ReparsePoint, StandardInformation, classify,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};
use support::fixture::{EntryKind, Fixture};

type NameKey = (u64, u16, Vec<u16>, u8);
#[derive(Debug, Default)]
struct Stream {
    size: Option<u64>,
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
    streams: BTreeMap<Vec<u16>, Stream>,
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
            AttributeType::DATA => {
                let stream = result
                    .streams
                    .entry(attribute.name().units().collect())
                    .or_default();
                stream.resident = attribute.is_resident();
                stream.sparse = attribute.flags().contains(AttributeFlags::SPARSE);
                stream.compressed = attribute.flags().compression_format() != 0;
                if let Some(value) = attribute.resident_value() {
                    assert!(stream.size.replace(value.len() as u64).is_none());
                } else {
                    let descriptor = attribute.non_resident().unwrap();
                    if let Some(sizes) = descriptor.sizes() {
                        assert!(stream.size.replace(sizes.data()).is_none());
                    }
                    for run in descriptor.runs() {
                        run.expect("fixture mapping pairs must parse");
                        stream.runs += 1;
                    }
                }
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

fn verify(name: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("../../tests/fixtures/ntfs/{name}.json"));
    let fixture = Fixture::load(&path).unwrap_or_else(|error| panic!("{name}: {error}"));
    assert_eq!(fixture.extents, fixture.manifest.mft.extents);
    let mut observed: BTreeMap<u64, Observed> = BTreeMap::new();
    let mut extensions = Vec::new();
    for (number, bytes) in fixture.records().enumerate() {
        let mut raw = bytes.to_vec();
        match classify(&mut raw) {
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
    for entry in &fixture.manifest.entries {
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
            actual.streams.len(),
            entry.streams.len(),
            "{} stream count",
            entry.path
        );
        for expected in &entry.streams {
            let stream = &actual.streams[&expected.name];
            assert_eq!(
                (
                    stream.size,
                    stream.resident,
                    stream.sparse,
                    stream.compressed,
                    stream.runs
                ),
                (
                    Some(expected.size),
                    expected.resident,
                    expected.sparse,
                    expected.compressed,
                    expected.run_count
                ),
                "{} stream {:?}",
                entry.path,
                expected.name
            );
        }
        if entry.path == "/bigdir" {
            assert!(actual.has_index);
        }
    }
    for fill in &fixture.manifest.fill {
        let prefix: Vec<_> = fill.name_prefix.encode_utf16().collect();
        let parent = FileRef::new(fill.parent_record, fill.parent_seq).unwrap();
        let mut count = 0;
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
            let mut raw = fixture.record(number).unwrap().to_vec();
            assert!(
                matches!(classify(&mut raw), RecordClass::Base(_)),
                "{} record {number} is not Base",
                fill.dir
            );
            for name in belongs {
                assert_eq!(name.1, parent.sequence());
                assert_eq!(name.2.len(), fill.name_len);
                assert!(name.2.starts_with(&prefix));
            }
            count += 1;
        }
        assert_eq!(count, fill.count, "{} filler count", fill.dir);
    }
    let root = &observed[&5];
    assert!(root.names.iter().any(|name| name.0 == 5 && name.1 == 5));
    assert_eq!(
        observed[&0].has_list,
        fixture.manifest.mft.has_attribute_list
    );
    for deleted in &fixture.manifest.deleted {
        let mut raw = fixture.record(deleted.record).unwrap().to_vec();
        assert!(
            matches!(classify(&mut raw), RecordClass::NotInUse(_)),
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
