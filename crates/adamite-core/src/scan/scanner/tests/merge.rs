//! Merge phase regressions.

use super::support::{DriverOutcome, TestResult, drive_to, ingest_volume, named_record};
use crate::graph::entry::EntryRecord;
use crate::scan::{ScanConfig, ScanPhase, Scanner};
use crate::snapshot::NodeFlags;
use adamite_fixtures::builders::{AttrBuilder, NonResidentModel, RecordBuilder};
use adamite_fixtures::{Fixture, MappedAllocation, ReferenceMft};
use std::error::Error;
use std::path::Path;

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
            highest: 1,
            sizes: [4096, 2048, 1024, 0],
            mapping_pairs: vec![0x11, 1, 0, 0x01, 1, 0],
            ..NonResidentModel::default()
        },
    );
    let mut continuation = AttrBuilder::non_resident(
        0x80,
        NonResidentModel {
            lowest: 2,
            highest: 3,
            sizes: [-1; 4],
            mapping_pairs: vec![0x01, 1, 0x11, 1, 4, 0],
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
    assert_eq!((entry.size, entry.ads_size, entry.alloc), (2048, 0, 1024));
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
    let allocation = MappedAllocation::from_reference(&ReferenceMft::from_fixture(&fixture)?)?;
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
            alloc = alloc
                .checked_add(allocation.expected_stream_bytes(expected.record, stream)?)
                .ok_or("manifest allocation total")?;
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
