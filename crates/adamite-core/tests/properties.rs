//! Public protocol properties over bounded, independently encoded NTFS volumes.

use adamite_core::{
    error::ScanError,
    scan::{FeedError, ReadRequest, ScanConfig, ScanOutput, ScanStep, Scanner},
    snapshot::{NodeFlags, NodeId, NodeView, Snapshot},
};
use adamite_fixtures::{
    builders::{
        AttrBody, AttrBuilder, AttrListBuilder, AttrListEntryModel, BootSectorBuilder,
        FileNameBuilder, NonResidentModel, RecordBuilder,
    },
    runlist_encoder::{RunModel, RunlistEncoder},
};
use proptest::{prelude::*, test_runner::Config};
use std::{cmp::Reverse, error::Error};

type TestResult<T = ()> = Result<T, Box<dyn Error>>;
// One-sector clusters and two-sector records let odd extent boundaries split records.
const SECTOR: usize = 512;
const RECORD_SIZE: usize = 1024;
const RECORDS: usize = 32;
const IMAGE_SECTORS: usize = 512;
const SEQUENCE: u64 = 7;
const FIRST_DIRECTORY: u32 = 16;
const FIRST_FILE: u32 = 24;
const DATA: u32 = 0x80;
const CHUNKS: [usize; 4] = [512, 1024, 4096, 65536];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bootstrap {
    Contiguous,
    Multipass,
    Stalled,
}

#[derive(Debug, Clone)]
struct VolumeCase {
    // Selectors are reduced against already-created directories, so shrinking stays valid.
    directories: Vec<u8>,
    files: Vec<(u8, u16, u8)>,
    bootstrap: Bootstrap,
}

fn volumes() -> impl Strategy<Value = VolumeCase> {
    (
        prop::collection::vec(any::<u8>(), 1..=4),
        prop::collection::vec((any::<u8>(), 0u16..=64, any::<u8>()), 2..=6),
        any::<bool>(),
    )
        .prop_map(|(directories, files, fragmented)| VolumeCase {
            directories,
            files,
            bootstrap: if fragmented {
                Bootstrap::Multipass
            } else {
                Bootstrap::Contiguous
            },
        })
}

fn file_ref(record: u32) -> u64 {
    (SEQUENCE << 48) | u64::from(record)
}

fn name(parent: u32, text: &str, namespace: u8, id: u16) -> Vec<u8> {
    let mut attribute = AttrBuilder::resident(
        0x30,
        FileNameBuilder {
            parent: file_ref(parent),
            namespace,
            name: text.encode_utf16().collect(),
            ..FileNameBuilder::default()
        }
        .build(),
    );
    attribute.id = id;
    attribute.build()
}

fn non_resident(id: u16, lowest: i64, run: RunModel, size: i64) -> AttrBuilder {
    let mut attribute = AttrBuilder::non_resident(
        DATA,
        NonResidentModel {
            lowest,
            highest: lowest + i64::try_from(run.len).expect("bounded run") - 1,
            sizes: [
                i64::try_from(run.len).expect("bounded run") * 512,
                size,
                size,
                0,
            ],
            mapping_pairs: RunlistEncoder { runs: vec![run] }.encode(0),
            ..NonResidentModel::default()
        },
    );
    attribute.id = id;
    attribute
}

fn mft_runs(bootstrap: Bootstrap) -> Vec<RunModel> {
    if bootstrap == Bootstrap::Contiguous {
        vec![RunModel {
            lcn: Some(4),
            len: 64,
        }]
    } else {
        vec![
            RunModel {
                lcn: Some(4),
                len: 17,
            },
            RunModel {
                lcn: Some(128),
                len: 16,
            },
            RunModel {
                lcn: Some(256),
                len: 31,
            },
        ]
    }
}

// Write through the independently specified layout, including records spanning extents.
fn put_record(image: &mut [u8], runs: &[RunModel], record: RecordBuilder) {
    let bytes = record.build();
    let start = usize::try_from(record.record_number).expect("bounded record") * RECORD_SIZE;
    let mut virtual_start = 0;
    for run in runs {
        let run_len = usize::try_from(run.len).expect("bounded run") * SECTOR;
        let overlap_start = start.max(virtual_start);
        let overlap_end = (start + bytes.len()).min(virtual_start + run_len);
        if overlap_start < overlap_end {
            let physical =
                usize::try_from(run.lcn.expect("non-sparse layout")).expect("bounded LCN") * SECTOR
                    + overlap_start
                    - virtual_start;
            let count = overlap_end - overlap_start;
            image[physical..physical + count]
                .copy_from_slice(&bytes[overlap_start - start..overlap_end - start]);
        }
        virtual_start += run_len;
    }
    assert_eq!(virtual_start, RECORDS * RECORD_SIZE);
}

fn bootstrap_records(image: &mut [u8], case: &VolumeCase, runs: &[RunModel]) {
    let mft_size = i64::try_from(RECORDS * RECORD_SIZE).expect("bounded MFT");
    let mut first_segment = non_resident(0, 0, runs[0], mft_size);
    if let AttrBody::NonResident(model) = &mut first_segment.body {
        // First-segment sizes describe the entire stream, including later extents.
        model.sizes[0] = mft_size;
    }
    let mut attributes = vec![first_segment.build()];
    if case.bootstrap != Bootstrap::Contiguous {
        let mut references = vec![AttrListEntryModel {
            type_code: DATA,
            lowest: 33,
            segment: file_ref(10),
            id: 2,
            name: vec![],
        }];
        if case.bootstrap != Bootstrap::Stalled {
            // List the unreachable target first to exercise a second discovery pass.
            references.push(AttrListEntryModel {
                type_code: DATA,
                lowest: 17,
                segment: file_ref(1),
                id: 1,
                name: vec![],
            });
        }
        let mut list = AttrBuilder::resident(
            0x20,
            AttrListBuilder {
                entries: references,
                zero_tail: 0,
            }
            .build(),
        );
        list.id = 3;
        attributes.push(list.build());
        for (record_number, id, lowest, run) in [(1, 1, 17, runs[1]), (10, 2, 33, runs[2])] {
            put_record(
                image,
                runs,
                RecordBuilder {
                    record_number,
                    base: file_ref(0),
                    attributes: vec![non_resident(id, lowest, run, 0).build()],
                    ..RecordBuilder::default()
                },
            );
        }
    }
    attributes.push(name(5, "$MFT", 1, 4));
    put_record(
        image,
        runs,
        RecordBuilder {
            record_number: 0,
            attributes,
            ..RecordBuilder::default()
        },
    );
}

fn volume(case: &VolumeCase) -> Vec<u8> {
    let mut image = vec![0; IMAGE_SECTORS * SECTOR];
    assert!(image.len() <= 1_048_576);
    image[..SECTOR].copy_from_slice(
        &BootSectorBuilder {
            cluster_encoding: 1,
            sectors: IMAGE_SECTORS as u64,
            mirror: 64,
            ..BootSectorBuilder::default()
        }
        .build(),
    );
    let runs = mft_runs(case.bootstrap);
    bootstrap_records(&mut image, case, &runs);
    put_record(
        &mut image,
        &runs,
        RecordBuilder {
            record_number: 5,
            flags: 3,
            attributes: vec![name(5, ".", 1, 0)],
            ..RecordBuilder::default()
        },
    );
    // Depth <= 3 and fan-out <= 4 for directories; files add at most 18 links per parent.
    let mut parents = vec![(5, 0)];
    for (index, selector) in case.directories.iter().enumerate() {
        let eligible: Vec<_> = parents
            .iter()
            .copied()
            .filter(|(_, depth)| *depth < 3)
            .collect();
        let (parent, depth) = eligible[usize::from(*selector) % eligible.len()];
        let number = FIRST_DIRECTORY + u32::try_from(index).expect("bounded directories");
        let mut index_allocation = non_resident(
            1,
            0,
            RunModel {
                len: 1,
                lcn: Some(390 + u64::from(number)),
            },
            512,
        );
        index_allocation.type_code = 0xa0;
        index_allocation.name = "$I30".encode_utf16().collect();
        put_record(
            &mut image,
            &runs,
            RecordBuilder {
                record_number: number,
                flags: 3,
                attributes: vec![
                    name(parent, &format!("dir{index}"), 1, 0),
                    index_allocation.build(),
                ],
                ..RecordBuilder::default()
            },
        );
        parents.push((number, depth + 1));
    }
    for (index, &(selector, size, options)) in case.files.iter().enumerate() {
        let number = FIRST_FILE + u32::try_from(index).expect("bounded files");
        let parent = parents[usize::from(selector) % parents.len()].0;
        let text = if options & 1 == 0 {
            format!("file{index}.TXT")
        } else {
            format!("文件🦀{index}.gz")
        };
        let mut attributes = vec![name(parent, &text, 1, 0)];
        let mut hard_links = 1;
        if index == 0 || options & 2 != 0 {
            attributes.push(name(parent, &format!("alias{index}.bin"), 0, 1));
            attributes.push(name(
                parents[(usize::from(selector) + 1) % parents.len()].0,
                &format!("cross{index}.txt"),
                0,
                2,
            ));
            hard_links = 3;
        }
        attributes.push(name(parent, &format!("FILE{index}~1.TXT"), 2, 3));
        let mut data = if index % 2 == 0 {
            AttrBuilder::resident(DATA, vec![options; usize::from(size)])
        } else {
            non_resident(
                4,
                0,
                RunModel {
                    len: 1,
                    lcn: Some(440 + u64::from(number)),
                },
                i64::from(size),
            )
        };
        data.id = 4;
        attributes.push(data.build());
        let mut ads = AttrBuilder::resident(DATA, vec![selector; usize::from(size % 17) + 1]);
        ads.id = 5;
        ads.name = "notes".encode_utf16().collect();
        if index == 0 {
            // Source ordinal 2 precedes base 24, independently of MFT bootstrap extensions.
            put_record(
                &mut image,
                &runs,
                RecordBuilder {
                    record_number: 2,
                    base: file_ref(number),
                    attributes: vec![ads.build()],
                    ..RecordBuilder::default()
                },
            );
        } else {
            attributes.push(ads.build());
        }
        put_record(
            &mut image,
            &runs,
            RecordBuilder {
                record_number: number,
                attributes,
                hard_links,
                ..RecordBuilder::default()
            },
        );
    }
    image
}

#[derive(Debug, Clone, Copy)]
enum Arrival<'a> {
    Issued,
    // Ranks 0..24 enumerate every permutation of up to four pending requests.
    Permuted(&'a [usize]),
}

fn permute(requests: &mut Vec<ReadRequest>, mut rank: usize) {
    let mut remaining = std::mem::take(requests);
    while !remaining.is_empty() {
        let factorial: usize = (1..remaining.len()).product();
        let index = (rank / factorial) % remaining.len();
        rank %= factorial;
        requests.push(remaining.remove(index));
    }
}

struct Driven {
    outcome: Result<ScanOutput, ScanError>,
    reads: Vec<ReadRequest>,
    calls: usize,
    reordered_batches: usize,
}

fn step_bound(image: &[u8]) -> usize {
    4096 + 64 * RECORDS + 16 * (image.len() / SECTOR)
}

// Accepted responses still occupy slots until consumed. Rejected empty feeds observe
// that lifetime through the public API without adding instrumentation to Scanner.
fn retained_requests(scanner: &mut Scanner, accepted: &mut Vec<ReadRequest>) -> TestResult {
    let mut live = Vec::new();
    for request in accepted.drain(..) {
        match scanner.feed(request.id, &[]) {
            Err(FeedError::DuplicateResponse { .. }) => live.push(request),
            Err(FeedError::UnknownRequest { .. } | FeedError::Inactive) => {}
            other => return Err(format!("unexpected accepted-slot probe: {other:?}").into()),
        }
    }
    *accepted = live;
    Ok(())
}

fn drive(image: &[u8], config: ScanConfig, arrival: Arrival<'_>) -> TestResult<Driven> {
    let cap = config.max_outstanding;
    let chunk = config.chunk_size;
    let mut scanner = Scanner::new(config)?;
    let mut pending = Vec::<ReadRequest>::new();
    let mut accepted = Vec::<ReadRequest>::new();
    let mut reads = Vec::<ReadRequest>::new();
    let mut batches = 0;
    let mut reordered_batches = 0;
    for calls in 1..=step_bound(image) {
        let step = scanner.step();
        retained_requests(&mut scanner, &mut accepted)?;
        if let Ok(ScanStep::Need(request)) = &step {
            assert!(
                request.len > 0 && request.len <= chunk,
                "request {request:?}"
            );
            let start = usize::try_from(request.offset)?;
            assert!(
                start
                    .checked_add(request.len)
                    .is_some_and(|end| end <= image.len()),
                "request {request:?}"
            );
            assert!(reads.last().is_none_or(|previous| previous.id < request.id));
            pending.push(*request);
            reads.push(*request);
        }
        assert!(
            pending.len() + accepted.len() <= cap,
            "outstanding slots exceeded cap"
        );
        let outstanding_bytes: usize = pending
            .iter()
            .chain(&accepted)
            .map(|request| request.len)
            .sum();
        assert!(
            outstanding_bytes <= cap * chunk,
            "outstanding bytes exceeded bound"
        );
        match step {
            Ok(ScanStep::Need(_)) => {}
            Ok(ScanStep::Progress(progress)) => {
                assert_eq!(progress.outstanding, pending.len() + accepted.len());
                if progress.waiting_for_input {
                    assert!(!pending.is_empty(), "waiting with no requests");
                    if let Arrival::Permuted(ranks) = arrival {
                        let before: Vec<_> = pending.iter().map(|request| request.id).collect();
                        permute(&mut pending, ranks[batches % ranks.len()]);
                        if before
                            .iter()
                            .copied()
                            .ne(pending.iter().map(|request| request.id))
                        {
                            reordered_batches += 1;
                        }
                    }
                    batches += 1;
                    for request in pending.drain(..) {
                        let start = usize::try_from(request.offset)?;
                        scanner.feed(request.id, &image[start..start + request.len])?;
                        accepted.push(request);
                    }
                }
            }
            Ok(ScanStep::Done(output)) => {
                assert!(pending.is_empty() && accepted.is_empty());
                return Ok(Driven {
                    outcome: Ok(output),
                    reads,
                    calls,
                    reordered_batches,
                });
            }
            Err(error) => {
                return Ok(Driven {
                    outcome: Err(error),
                    reads,
                    calls,
                    reordered_batches,
                });
            }
            Ok(_) => return Err("unknown scan step".into()),
        }
    }
    Err("scan exceeded its record/sector-derived step bound".into())
}

fn node(snapshot: &Snapshot, id: NodeId) -> NodeView<'_> {
    snapshot.node(id).expect("node in generated snapshot")
}

fn same_snapshot(left: &Snapshot, right: &Snapshot) -> TestResult {
    assert_eq!(left.len(), right.len());
    assert_eq!(left.is_empty(), right.is_empty());
    assert_eq!(left.volume(), right.volume());
    for raw in 0..u32::try_from(left.len())? {
        let id = NodeId::from_raw(raw);
        // NodeView equality includes every hot field and cold column through a public value.
        assert_eq!(left.node(id), right.node(id), "node {raw}");
        assert_eq!(left.name(id), right.name(id), "name {raw}");
        assert_eq!(left.path(id)?, right.path(id)?, "path {raw}");
        assert_eq!(left.children(id), right.children(id), "children {raw}");
    }
    assert_eq!(left.top_files(), right.top_files());
    assert_eq!(left.ext_stats(), right.ext_stats());
    Ok(())
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct ChildKey {
    allocation: Reverse<u64>,
    name: Vec<u16>,
    file_ref: Option<(u64, u16)>,
    id: NodeId,
}

fn child_key(snapshot: &Snapshot, id: NodeId) -> ChildKey {
    let view = node(snapshot, id);
    ChildKey {
        allocation: Reverse(view.total_alloc()),
        name: snapshot
            .name(id)
            .expect("pooled name")
            .encode_utf16()
            .collect(),
        file_ref: view
            .file_ref()
            .map(|reference| (reference.record(), reference.sequence())),
        id,
    }
}

fn aggregation_invariants(snapshot: &Snapshot) -> TestResult {
    let mut seen = vec![0; snapshot.len()];
    let mut csr_cursor = 0;
    for raw in 0..u32::try_from(snapshot.len())? {
        let id = NodeId::from_raw(raw);
        let view = node(snapshot, id);
        let children = snapshot.children(id).expect("CSR range");
        assert_eq!(
            usize::try_from(view.child_start())?,
            csr_cursor,
            "CSR start at {raw}"
        );
        assert_eq!(usize::try_from(view.child_len())?, children.len());
        csr_cursor += children.len();
        assert!(
            children
                .windows(2)
                .all(|pair| child_key(snapshot, pair[0]) <= child_key(snapshot, pair[1])),
            "child order at {raw}"
        );
        let mut size = view.size();
        let mut alloc = view.alloc();
        let mut files = u32::from(
            !view.is_dir()
                && !view.flags().contains(NodeFlags::SYNTHETIC)
                && !view.flags().contains(NodeFlags::HARDLINK_DUP),
        );
        for &child in children {
            seen[usize::try_from(child.raw())?] += 1;
            let contribution = node(snapshot, child);
            assert_eq!(contribution.parent(), id, "CSR parent of {child:?}");
            if !contribution.flags().contains(NodeFlags::HARDLINK_DUP) {
                size += contribution.total_size();
                alloc += contribution.total_alloc();
                files += contribution.file_count();
            }
        }
        assert_eq!(
            (view.total_size(), view.total_alloc(), view.file_count()),
            (size, alloc, files),
            "totals at {raw}"
        );
    }
    assert_eq!(csr_cursor, snapshot.len() - 1);
    assert_eq!(seen[0], 0);
    assert!(
        seen[1..].iter().all(|&count| count == 1),
        "CSR must contain each non-root once"
    );
    Ok(())
}

fn proptest_config() -> Config {
    let mut config = Config::default();
    if std::env::var_os("PROPTEST_CASES").is_none() {
        config.cases = 32;
    }
    config
}

proptest! {
    #![proptest_config(proptest_config())]
    #[test]
    fn chunk_order_transport_termination_and_aggregation(
        case in volumes(),
        ranks in prop::collection::vec(0usize..24, 1..=32),
    ) {
        let image = volume(&case);
        let reference = drive(&image, ScanConfig::default(), Arrival::Issued)
            .map_err(|error| TestCaseError::fail(error.to_string()))?;
        let reference = reference.outcome.map_err(|error| TestCaseError::fail(error.to_string()))?;
        prop_assert_eq!(reference.stats.records_total, RECORDS as u64);
        prop_assert_eq!(reference.stats.corrupt_records, 0);
        prop_assert_eq!(reference.stats.corrupt_streams, 0);
        prop_assert_eq!(reference.stats.rejected_extensions, 0);
        let bases = 2 + case.directories.len() + case.files.len();
        let extensions = if case.bootstrap == Bootstrap::Multipass { 3 } else { 1 };
        prop_assert_eq!(reference.stats.base_records, bases as u64);
        prop_assert_eq!(reference.stats.extension_records, extensions);
        prop_assert_eq!(reference.stats.records_in_use, bases as u64 + extensions);
        let duplicates = case.files.iter().enumerate()
            .filter(|(index, (_, _, options))| *index == 0 || options & 2 != 0).count() * 2;
        prop_assert_eq!(reference.stats.hardlink_duplicates, duplicates as u64);
        aggregation_invariants(&reference.snapshot)
            .map_err(|error| TestCaseError::fail(error.to_string()))?;
        for chunk in CHUNKS.into_iter().chain([ScanConfig::default().chunk_size]) {
            for cap in 1..=4 {
                let varied = drive(&image, ScanConfig { chunk_size: chunk, max_outstanding: cap, ..ScanConfig::default() }, Arrival::Permuted(&ranks))
                    .map_err(|error| TestCaseError::fail(error.to_string()))?;
                prop_assert!(varied.calls <= step_bound(&image));
                let varied = varied.outcome.map_err(|error| TestCaseError::fail(error.to_string()))?;
                same_snapshot(&reference.snapshot, &varied.snapshot)
                    .map_err(|error| TestCaseError::fail(error.to_string()))?;
                prop_assert_eq!(&reference.stats, &varied.stats, "chunk {}, cap {}", chunk, cap);
                aggregation_invariants(&varied.snapshot)
                    .map_err(|error| TestCaseError::fail(error.to_string()))?;
            }
        }
    }
}

fn minimal_case(bootstrap: Bootstrap) -> VolumeCase {
    VolumeCase {
        directories: vec![0],
        files: vec![(0, 3, 0), (1, 7, 1)],
        bootstrap,
    }
}

#[test]
fn bootstrap_ordering_reaches_later_extension_only_after_earlier_runs() -> TestResult {
    let image = volume(&minimal_case(Bootstrap::Multipass));
    let driven = drive(
        &image,
        ScanConfig {
            chunk_size: 512,
            max_outstanding: 4,
            ..ScanConfig::default()
        },
        Arrival::Permuted(&[23]),
    )?;
    let first = driven
        .reads
        .iter()
        .position(|request| request.offset == 6 * 512)
        .expect("first bootstrap extension");
    let later = driven
        .reads
        .iter()
        .position(|request| request.offset == 131 * 512)
        .expect("second bootstrap extension");
    assert!(
        first < later,
        "unmapped extension must wait for its predecessor"
    );
    assert!(
        driven.reordered_batches > 0,
        "exercise a changed response order"
    );
    let output = driven.outcome?;
    assert_eq!(output.stats.extension_records, 3);
    assert_eq!(output.stats.records_total, RECORDS as u64);
    assert!(driven.calls <= step_bound(&image));
    aggregation_invariants(&output.snapshot)
}

#[test]
fn merge_ordering_extension_precedes_its_base() -> TestResult {
    let image = volume(&minimal_case(Bootstrap::Contiguous));
    let output = drive(&image, ScanConfig::default(), Arrival::Issued)?.outcome?;
    let mut links = Vec::new();
    for raw in 0..u32::try_from(output.snapshot.len())? {
        let view = node(&output.snapshot, NodeId::from_raw(raw));
        assert_ne!(view.file_ref().map(|reference| reference.record()), Some(2));
        if view
            .file_ref()
            .is_some_and(|reference| reference.record() == u64::from(FIRST_FILE))
        {
            assert_eq!(view.ads_size(), 4, "ADS from the earlier extension");
            assert_eq!(view.size(), 3);
            assert_eq!(view.nlink(), 3);
            links.push(view);
        }
    }
    assert_eq!(links.len(), 3);
    assert_eq!(
        links
            .iter()
            .filter(|view| !view.flags().contains(NodeFlags::HARDLINK_DUP))
            .count(),
        1
    );
    assert_eq!(output.stats.rejected_extensions, 0);
    Ok(())
}

#[test]
fn no_progress_terminates_with_bootstrap_incomplete() -> TestResult {
    let image = volume(&minimal_case(Bootstrap::Stalled));
    for cap in 1..=4 {
        let driven = drive(
            &image,
            ScanConfig {
                chunk_size: 512,
                max_outstanding: cap,
                ..ScanConfig::default()
            },
            Arrival::Permuted(&[23]),
        )?;
        assert!(matches!(
            driven.outcome,
            Err(ScanError::BootstrapIncomplete)
        ));
        assert!(driven.calls <= step_bound(&image));
        assert!(
            driven.reads.iter().all(|request| request.offset < 21 * 512),
            "never read the unreachable target"
        );
    }
    Ok(())
}
