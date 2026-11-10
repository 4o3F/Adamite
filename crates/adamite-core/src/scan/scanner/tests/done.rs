//! Done phase regressions.

use super::support::{TestResult, drive_to};
use crate::error::ScanError;
use crate::scan::{FeedError, RequestId, ScanConfig, ScanPhase, ScanStep, Scanner};
use adamite_fixtures::Fixture;
use std::error::Error;
use std::path::Path;

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
