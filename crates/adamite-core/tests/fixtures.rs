//! Public end-to-end scans checked against independent v3 NTFS fixture manifests.

use adamite_core::{
    error::ScanError,
    scan::{
        FeedError, ReadRequest, RequestId, ScanConfig, ScanOutput, ScanPhase, ScanStep, Scanner,
    },
    snapshot::{NodeFlags, NodeId, NodeView, Snapshot},
};
use adamite_fixtures::{Entry, EntryKind, Fixture, Manifest, MappedAllocation, Name, ReferenceMft};
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    path::Path,
};

type TestResult<T = ()> = Result<T, Box<dyn Error>>;
const METADATA: &str = "/<NTFS metadata>";
// NTFS wire values are stated independently of the parser's attribute helpers.
const DATA_ATTRIBUTE: u32 = 0x80;
const DIRECTORY_ATTRIBUTE: u32 = 0x10;
const REPARSE_ATTRIBUTE: u32 = 0x400;
const SPARSE_ATTRIBUTE: u32 = 0x200;
const COMPRESSED_ATTRIBUTE: u32 = 0x800;
const HIDDEN_ATTRIBUTE: u32 = 2;
const SYSTEM_ATTRIBUTE: u32 = 4;

#[derive(Clone, Copy)]
enum Arrival {
    Issued,
    Reversed,
}
struct Driven {
    output: ScanOutput,
    reads: Vec<ReadRequest>,
    reversed_batches: usize,
}

// This driver deliberately has no manifest parameter: all addresses come from Scanner.
fn scan(
    image: &[u8],
    config: ScanConfig,
    arrival: Arrival,
    records_planned: u64,
) -> TestResult<Driven> {
    let chunk_size = config.chunk_size;
    let max_outstanding = config.max_outstanding;
    let mut scanner = Scanner::new(config)?;
    let mut pending = Vec::<ReadRequest>::new();
    let mut reads = Vec::<ReadRequest>::new();
    let mut reversed_batches = 0;
    let limit = image
        .len()
        .checked_div(512)
        .and_then(|n| n.checked_mul(16))
        .and_then(|n| n.checked_add(4096))
        .ok_or("driver call bound")?;
    let mut previous_records = 0;
    let mut phases = Vec::new();
    for _ in 0..limit {
        match scanner.step()? {
            ScanStep::Need(request) => {
                assert!(request.len > 0 && request.len <= chunk_size);
                if let Some(previous) = reads.last() {
                    assert!(request.id > previous.id);
                }
                pending.push(request);
                reads.push(request);
                assert!(pending.len() <= max_outstanding);
            }
            ScanStep::Progress(progress) => {
                if phases.last() != Some(&progress.phase) {
                    phases.push(progress.phase);
                }
                let expected = match progress.phase {
                    ScanPhase::Boot | ScanPhase::Record0 | ScanPhase::Discover => None,
                    _ => Some(records_planned),
                };
                assert_eq!(
                    progress.records_planned, expected,
                    "phase {:?}",
                    progress.phase
                );
                assert!(progress.records_processed <= records_planned);
                assert!(
                    progress
                        .records_processed
                        .checked_sub(previous_records)
                        .ok_or("record count decreased")?
                        <= 256
                );
                previous_records = progress.records_processed;
                assert!(progress.outstanding <= max_outstanding);
                if progress.waiting_for_input {
                    assert!(!pending.is_empty(), "waiting without a read request");
                    if matches!(arrival, Arrival::Reversed) {
                        if pending.len() > 1 {
                            reversed_batches += 1;
                        }
                        pending.reverse();
                    }
                    for request in pending.drain(..) {
                        let start = usize::try_from(request.offset)?;
                        let end = start.checked_add(request.len).ok_or("read end overflow")?;
                        scanner.feed(
                            request.id,
                            image.get(start..end).ok_or("read outside image")?,
                        )?;
                    }
                }
            }
            ScanStep::Done(output) => {
                assert_eq!(previous_records, records_planned);
                assert_eq!(output.stats.records_total, records_planned);
                assert_eq!(
                    phases,
                    [
                        ScanPhase::Boot,
                        ScanPhase::Record0,
                        ScanPhase::Discover,
                        ScanPhase::Stream,
                        ScanPhase::Merge,
                        ScanPhase::Link,
                        ScanPhase::Snapshot,
                        ScanPhase::Aggregate,
                        ScanPhase::Finalize,
                        ScanPhase::Done
                    ]
                );
                assert!(pending.is_empty());
                for _ in 0..2 {
                    assert!(matches!(scanner.step(), Err(ScanError::Inactive)));
                    assert!(matches!(
                        scanner.feed(RequestId::from_raw(0), &[]),
                        Err(FeedError::Inactive)
                    ));
                }
                return Ok(Driven {
                    output,
                    reads,
                    reversed_batches,
                });
            }
            _ => return Err("unknown scan step".into()),
        }
    }
    Err("scan exceeded its image-derived call bound".into())
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Sizes {
    size: u64,
    ads: u64,
    alloc: u64,
}
impl Sizes {
    fn add(&mut self, other: Self) {
        self.size += other.size;
        self.ads += other.ads;
        self.alloc += other.alloc;
    }
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Totals {
    size: u64,
    alloc: u64,
    files: u64,
}
impl Totals {
    fn add(&mut self, other: Self) {
        self.size += other.size;
        self.alloc += other.alloc;
        self.files += other.files;
    }
}
fn measurements(entry: &Entry, allocation: &MappedAllocation) -> TestResult<Sizes> {
    let mut result = Sizes::default();
    let mut unnamed = 0;
    for stream in &entry.streams {
        if stream.attribute_type == DATA_ATTRIBUTE {
            if stream.name.is_empty() {
                result.size = stream.size;
                unnamed += 1;
            } else if !(entry.record == 8 && stream.name == [36, 66, 97, 100]) {
                result.ads += stream.size;
            }
        }
        result.alloc += allocation.expected_stream_bytes(entry.record, stream)?;
    }
    assert!(
        unnamed <= 1,
        "manifest contains more than one unnamed DATA stream"
    );
    Ok(result)
}
fn directory(entry: &Entry) -> bool {
    matches!(entry.kind, EntryKind::Directory) || entry.attributes & DIRECTORY_ATTRIBUTE != 0
}
fn name_keys(entry: &Entry) -> BTreeSet<(u64, u16, &[u16], u8)> {
    entry
        .names
        .iter()
        .map(|name| {
            (
                name.parent_record,
                name.parent_seq,
                name.name.as_slice(),
                name.namespace,
            )
        })
        .collect()
}
fn retained_names(entry: &Entry) -> Vec<&Name> {
    // Group by exact parent and raw name; keep the least namespace for an identical link.
    let mut links = BTreeMap::<_, &Name>::new();
    for name in &entry.names {
        if name.namespace == 2
            && entry.names.iter().any(|other| {
                other.parent_record == name.parent_record
                    && other.parent_seq == name.parent_seq
                    && other.namespace != 2
            })
        {
            continue;
        }
        let key = (name.parent_record, name.parent_seq, &name.name);
        links
            .entry(key)
            .and_modify(|old| {
                if name.namespace < old.namespace {
                    *old = name;
                }
            })
            .or_insert(name);
    }
    let mut names: Vec<_> = links.into_values().collect();
    names.sort_by_key(|name| {
        (
            name.parent_record,
            &name.name,
            name.namespace,
            name.parent_seq,
        )
    });
    names
}
struct Link<'a> {
    entry: &'a Entry,
    metadata: bool,
    name: Vec<u16>,
    parent: u64,
    parent_seq: u16,
    duplicate: bool,
}
fn link_path(
    start: usize,
    links: &[Link<'_>],
    primaries: &BTreeMap<u64, usize>,
) -> TestResult<String> {
    let mut current = start;
    let mut parts = Vec::new();
    for _ in 0..=links.len() {
        let link = links.get(current).ok_or("oracle link index")?;
        if link.entry.record == 5 {
            parts.reverse();
            return Ok(format!("/{}", parts.join("/")));
        }
        parts.push(String::from_utf16_lossy(&link.name));
        if link.metadata {
            parts.reverse();
            return Ok(format!("{METADATA}/{}", parts.join("/")));
        }
        current = *primaries.get(&link.parent).ok_or("oracle parent missing")?;
        let parent = links.get(current).ok_or("oracle parent index")?;
        assert!(directory(parent.entry));
        assert_eq!(parent.entry.seq, link.parent_seq);
    }
    Err("cycle in manifest parent links".into())
}
fn parent_path(path: &str) -> TestResult<&str> {
    if path == "/" {
        return Ok("/");
    }
    let (parent, _) = path.rsplit_once('/').ok_or("non-absolute oracle path")?;
    Ok(if parent.is_empty() { "/" } else { parent })
}

struct Expected<'a> {
    entry: Option<&'a Entry>,
    reference: Option<(u64, u16)>,
    parent: String,
    name: Vec<u16>,
    sizes: Sizes,
    directory: bool,
    duplicate: bool,
    synthetic: bool,
    filler: bool,
}
struct Oracle<'a> {
    rows: BTreeMap<String, Expected<'a>>,
    totals: BTreeMap<String, Totals>,
}
fn explicit_rows<'a>(
    manifest: &'a Manifest,
    allocation: &MappedAllocation,
) -> TestResult<BTreeMap<String, Expected<'a>>> {
    let mut records = BTreeMap::<u64, (&Entry, bool)>::new();
    for entry in &manifest.entries {
        if let Some((previous, _)) = records.get(&entry.record) {
            assert_eq!(
                (
                    previous.seq,
                    previous.attributes,
                    previous.mtime_filetime,
                    previous.nlink,
                    previous.reparse_tag,
                    directory(previous)
                ),
                (
                    entry.seq,
                    entry.attributes,
                    entry.mtime_filetime,
                    entry.nlink,
                    entry.reparse_tag,
                    directory(entry)
                ),
                "shared inode metadata must agree for all manifest hard-link paths"
            );
            assert_eq!(
                measurements(previous, allocation)?,
                measurements(entry, allocation)?
            );
            assert_eq!(name_keys(previous), name_keys(entry));
            assert_eq!(
                (
                    previous.streams.iter().any(|stream| stream.sparse),
                    previous.streams.iter().any(|stream| stream.compressed)
                ),
                (
                    entry.streams.iter().any(|stream| stream.sparse),
                    entry.streams.iter().any(|stream| stream.compressed)
                )
            );
        } else {
            records.insert(entry.record, (entry, false));
        }
    }
    for entry in &manifest.metafiles {
        assert!(records.insert(entry.record, (entry, true)).is_none());
    }
    let root_seq = records.get(&5).ok_or("manifest root")?.0.seq;
    let mut links = Vec::new();
    let mut primaries = BTreeMap::new();
    for (record, (entry, metadata)) in records {
        primaries.insert(record, links.len());
        let names = retained_names(entry);
        if record == 5 || names.is_empty() {
            let name = if record == 5 {
                "/".into()
            } else {
                format!("<record {record}>")
            };
            links.push(Link {
                entry,
                metadata,
                name: name.encode_utf16().collect(),
                parent: 5,
                parent_seq: root_seq,
                duplicate: false,
            });
        } else {
            for (index, name) in names.into_iter().enumerate() {
                links.push(Link {
                    entry,
                    metadata,
                    name: name.name.clone(),
                    parent: name.parent_record,
                    parent_seq: name.parent_seq,
                    duplicate: index != 0,
                });
            }
        }
    }
    let mut rows = BTreeMap::new();
    for (index, link) in links.iter().enumerate() {
        let path = link_path(index, &links, &primaries)?;
        let row = Expected {
            entry: Some(link.entry),
            reference: Some((link.entry.record, link.entry.seq)),
            parent: parent_path(&path)?.into(),
            name: link.name.clone(),
            sizes: measurements(link.entry, allocation)?,
            directory: directory(link.entry),
            duplicate: link.duplicate,
            synthetic: false,
            filler: false,
        };
        assert!(rows.insert(path, row).is_none(), "duplicate oracle path");
    }
    if !manifest.metafiles.is_empty() {
        rows.insert(
            METADATA.into(),
            Expected {
                entry: None,
                reference: None,
                parent: "/".into(),
                name: "<NTFS metadata>".encode_utf16().collect(),
                sizes: Sizes::default(),
                directory: true,
                duplicate: false,
                synthetic: true,
                filler: false,
            },
        );
    }
    Ok(rows)
}
fn node(snapshot: &Snapshot, id: NodeId) -> TestResult<NodeView<'_>> {
    snapshot
        .node(id)
        .ok_or_else(|| "node id outside snapshot".into())
}
fn paths(snapshot: &Snapshot) -> TestResult<BTreeMap<String, NodeId>> {
    let mut result = BTreeMap::new();
    for raw in 0..u32::try_from(snapshot.len())? {
        let id = NodeId::from_raw(raw);
        let path = snapshot.path(id)?.ok_or("snapshot path missing")?;
        assert!(
            result.insert(path, id).is_none(),
            "two nodes resolve to the same path"
        );
    }
    Ok(result)
}
fn add_fill<'a>(
    manifest: &Manifest,
    snapshot: &Snapshot,
    paths: &BTreeMap<String, NodeId>,
    rows: &mut BTreeMap<String, Expected<'a>>,
) -> TestResult {
    let mut used: BTreeSet<_> = rows
        .values()
        .filter_map(|row| row.reference.map(|reference| reference.0))
        .collect();
    for fill in &manifest.fill {
        let parent = *paths.get(&fill.dir).ok_or("fill parent path")?;
        let reference = node(snapshot, parent)?
            .file_ref()
            .ok_or("fill parent reference")?;
        assert_eq!(
            (reference.record(), reference.sequence()),
            (fill.parent_record, fill.parent_seq)
        );
        let children: Vec<_> = paths
            .iter()
            .filter_map(|(path, id)| {
                snapshot
                    .node(*id)
                    .filter(|view| *id != NodeId::ROOT && view.parent() == parent)
                    .map(|view| (path, *id, view))
            })
            .collect();
        assert_eq!(children.len(), fill.count, "{} child count", fill.dir);
        assert_eq!(
            snapshot.children(parent).ok_or("fill children")?.len(),
            fill.count
        );
        let mut sum = Sizes::default();
        let mut records = BTreeSet::new();
        for (path, id, view) in children {
            let reference = view.file_ref().ok_or("filler reference")?;
            assert!((fill.record_min..=fill.record_max).contains(&reference.record()));
            assert!(
                records.insert(reference.record()) && used.insert(reference.record()),
                "filler base counted twice"
            );
            let name = snapshot.name(id).ok_or("filler name")?;
            assert!(name.starts_with(&fill.name_prefix));
            assert_eq!(name.encode_utf16().count(), fill.name_len);
            assert!(!view.is_dir());
            assert!(!view.flags().contains(NodeFlags::HARDLINK_DUP));
            assert!(!view.flags().contains(NodeFlags::SYNTHETIC));
            let sizes = Sizes {
                size: view.size(),
                ads: view.ads_size(),
                alloc: view.alloc(),
            };
            sum.add(sizes);
            assert!(
                rows.insert(
                    path.clone(),
                    Expected {
                        entry: None,
                        reference: Some((reference.record(), reference.sequence())),
                        parent: fill.dir.clone(),
                        name: name.encode_utf16().collect(),
                        sizes,
                        directory: false,
                        duplicate: false,
                        synthetic: false,
                        filler: true
                    }
                )
                .is_none()
            );
        }
        assert_eq!(records.first(), Some(&fill.record_min));
        assert_eq!(records.last(), Some(&fill.record_max));
        assert_eq!(
            sum,
            Sizes {
                size: fill.total_size.ok_or("fill size")?,
                ads: fill.total_ads_size.ok_or("fill ADS")?,
                alloc: fill.total_alloc.ok_or("fill allocation")?
            }
        );
    }
    Ok(())
}
fn add_ancestors(
    rows: &BTreeMap<String, Expected<'_>>,
    totals: &mut BTreeMap<String, Totals>,
    start: &str,
    contribution: Totals,
) -> TestResult {
    let mut current = start;
    for _ in 0..=rows.len() {
        totals
            .get_mut(current)
            .ok_or("oracle directory totals")?
            .add(contribution);
        if current == "/" {
            return Ok(());
        }
        current = &rows.get(current).ok_or("oracle ancestor")?.parent;
    }
    Err("oracle ancestor cycle".into())
}
impl<'a> Oracle<'a> {
    fn new(
        manifest: &'a Manifest,
        allocation: &MappedAllocation,
        snapshot: &Snapshot,
        paths: &BTreeMap<String, NodeId>,
    ) -> TestResult<Self> {
        let mut rows = explicit_rows(manifest, allocation)?;
        add_fill(manifest, snapshot, paths, &mut rows)?;
        let mut totals: BTreeMap<_, _> = rows
            .iter()
            .filter(|(_, row)| row.directory)
            .map(|(path, row)| {
                (
                    path.clone(),
                    Totals {
                        size: row.sizes.size,
                        alloc: row.sizes.alloc,
                        files: 0,
                    },
                )
            })
            .collect();
        for (path, row) in &rows {
            if path == "/" || row.duplicate || row.filler {
                continue;
            }
            add_ancestors(
                &rows,
                &mut totals,
                &row.parent,
                Totals {
                    size: row.sizes.size,
                    alloc: row.sizes.alloc,
                    files: u64::from(!row.directory && !row.synthetic),
                },
            )?;
        }
        // Directory totals use the manifest summaries, never observed filler self values.
        for fill in &manifest.fill {
            add_ancestors(
                &rows,
                &mut totals,
                &fill.dir,
                Totals {
                    size: fill.total_size.ok_or("fill logical total")?,
                    alloc: fill.total_alloc.ok_or("fill alloc total")?,
                    files: u64::try_from(fill.count)?,
                },
            )?;
        }
        Ok(Self { rows, totals })
    }
    fn total(&self, path: &str) -> TestResult<Totals> {
        let row = self.rows.get(path).ok_or("oracle row")?;
        Ok(if row.directory {
            *self.totals.get(path).ok_or("oracle directory")?
        } else {
            Totals {
                size: row.sizes.size,
                alloc: row.sizes.alloc,
                files: u64::from(!row.duplicate && !row.synthetic),
            }
        })
    }
}

fn compare_rows(
    oracle: &Oracle<'_>,
    manifest: &Manifest,
    snapshot: &Snapshot,
    paths: &BTreeMap<String, NodeId>,
) -> TestResult {
    assert_eq!(
        paths.len(),
        oracle.rows.len(),
        "every output node must be covered by the manifest"
    );
    for entry in &manifest.entries {
        assert!(
            paths.contains_key(&entry.path),
            "missing manifest path {}",
            entry.path
        );
    }
    for (path, expected) in &oracle.rows {
        let id = *paths
            .get(path)
            .ok_or_else(|| format!("missing expected path {path}"))?;
        let actual = node(snapshot, id)?;
        assert_eq!(
            actual
                .file_ref()
                .map(|reference| (reference.record(), reference.sequence())),
            expected.reference,
            "{path}: FileRef"
        );
        assert_eq!(
            snapshot.name(id).ok_or("name")?,
            String::from_utf16_lossy(&expected.name),
            "{path}: name"
        );
        assert_eq!(
            snapshot.path(actual.parent())?.as_deref(),
            Some(expected.parent.as_str()),
            "{path}: parent"
        );
        assert_eq!(
            Sizes {
                size: actual.size(),
                ads: actual.ads_size(),
                alloc: actual.alloc()
            },
            expected.sizes,
            "{path}: self values"
        );
        let total = oracle.total(path)?;
        assert_eq!(
            Totals {
                size: actual.total_size(),
                alloc: actual.total_alloc(),
                files: u64::from(actual.file_count())
            },
            total,
            "{path}: subtree totals"
        );
        if expected.filler {
            continue;
        }
        let entry = expected.entry;
        let flags = [
            (NodeFlags::DIRECTORY, expected.directory),
            (NodeFlags::SYNTHETIC, expected.synthetic),
            (NodeFlags::HARDLINK_DUP, expected.duplicate),
            (
                NodeFlags::REPARSE,
                entry.is_some_and(|e| {
                    e.reparse_tag.is_some() || e.attributes & REPARSE_ATTRIBUTE != 0
                }),
            ),
            (
                NodeFlags::SPARSE,
                entry.is_some_and(|e| {
                    e.attributes & SPARSE_ATTRIBUTE != 0 || e.streams.iter().any(|s| s.sparse)
                }),
            ),
            (
                NodeFlags::COMPRESSED,
                entry.is_some_and(|e| {
                    e.attributes & COMPRESSED_ATTRIBUTE != 0
                        || e.streams.iter().any(|s| s.compressed)
                }),
            ),
            (
                NodeFlags::HIDDEN,
                entry.is_some_and(|e| e.attributes & HIDDEN_ATTRIBUTE != 0),
            ),
            (
                NodeFlags::SYSTEM,
                entry.is_some_and(|e| e.attributes & SYSTEM_ATTRIBUTE != 0),
            ),
        ];
        let bits = flags
            .into_iter()
            .filter(|(_, set)| *set)
            .fold(0, |bits, (flag, _)| bits | flag.raw());
        assert_eq!(actual.flags().raw(), bits, "{path}: flags");
        assert_eq!(
            actual.attrs(),
            entry.map_or(0, |entry| entry.attributes),
            "{path}: SI attributes"
        );
        assert_eq!(
            actual.mtime(),
            entry.map_or(0, |entry| entry.mtime_filetime),
            "{path}: FILETIME"
        );
        assert_eq!(
            u64::from(actual.nlink()),
            entry.map_or(0, |entry| entry.nlink),
            "{path}: nlink"
        );
        assert_eq!(
            actual.reparse_tag(),
            entry.and_then(|entry| entry.reparse_tag),
            "{path}: reparse tag"
        );
        if entry.is_some_and(|entry| matches!(entry.kind, EntryKind::Symlink)) {
            assert!(actual.flags().contains(NodeFlags::REPARSE));
        }
    }
    for entry in &manifest.metafiles {
        let matching: Vec<_> = oracle
            .rows
            .iter()
            .filter(|(_, row)| row.reference == Some((entry.record, entry.seq)))
            .collect();
        assert!(!matching.is_empty());
        assert!(matching.iter().all(|(_, row)| row.parent == METADATA));
    }
    Ok(())
}

fn compare_indexes(
    oracle: &Oracle<'_>,
    snapshot: &Snapshot,
    paths: &BTreeMap<String, NodeId>,
) -> TestResult {
    // Keys come from oracle names and measured sizes, not from core's ranking helpers.
    let mut children = BTreeMap::<&str, Vec<&str>>::new();
    for (path, row) in &oracle.rows {
        if path != "/" {
            children.entry(&row.parent).or_default().push(path);
        }
    }
    let mut csr_end = 0usize;
    let mut seen = BTreeSet::new();
    for raw in 0..u32::try_from(snapshot.len())? {
        let id = NodeId::from_raw(raw);
        let view = node(snapshot, id)?;
        let path = snapshot.path(id)?.ok_or("CSR parent path")?;
        let expected = children.remove(path.as_str()).unwrap_or_default();
        let mut ranked = Vec::new();
        for child in expected {
            let row = oracle.rows.get(child).ok_or("missing child oracle row")?;
            ranked.push((
                std::cmp::Reverse(oracle.total(child)?.alloc),
                &row.name,
                row.reference,
                *paths.get(child).ok_or("missing child path")?,
            ));
        }
        ranked.sort_unstable();
        let expected_ids: Vec<_> = ranked.into_iter().map(|(_, _, _, id)| id).collect();
        let actual = snapshot.children(id).ok_or("CSR child range")?;
        assert_eq!(actual, expected_ids, "{path}: child order");
        assert_eq!(usize::try_from(view.child_start())?, csr_end);
        assert_eq!(usize::try_from(view.child_len())?, actual.len());
        csr_end += actual.len();
        for child in actual {
            assert!(seen.insert(*child), "child stored more than once");
            assert_eq!(node(snapshot, *child)?.parent(), id);
            assert_ne!(*child, NodeId::ROOT);
        }
    }
    assert!(children.is_empty());
    assert_eq!(csr_end + 1, snapshot.len());
    assert_eq!(seen.len(), csr_end);

    let mut primary_files: Vec<_> = oracle
        .rows
        .iter()
        .filter(|(_, row)| !row.directory && !row.duplicate && !row.synthetic)
        .collect();
    primary_files.sort_by_key(|(path, row)| {
        (
            std::cmp::Reverse(row.sizes.alloc),
            &row.name,
            row.reference,
            paths[*path],
        )
    });
    let expected_top: Vec<_> = primary_files
        .iter()
        .take(100)
        .map(|(path, _)| paths[*path])
        .collect();
    assert_eq!(snapshot.top_files(), expected_top);

    // Fill summaries omit individual streams. Their self values have already been checked
    // against the independent group totals; their public names identify extension groups.
    let mut extensions = BTreeMap::<String, Totals>::new();
    for (_, row) in primary_files {
        let name = String::from_utf16_lossy(&row.name);
        let dot = name.rfind('.');
        let suffix = match dot {
            Some(dot) if dot > 0 && dot + 1 < name.len() => &name[dot + 1..],
            _ => "",
        };
        extensions
            .entry(suffix.to_ascii_lowercase())
            .or_default()
            .add(Totals {
                size: row.sizes.size,
                alloc: row.sizes.alloc,
                files: 1,
            });
    }
    let mut expected: Vec<_> = extensions.into_iter().collect();
    expected.sort_by(|(left_ext, left), (right_ext, right)| {
        right.alloc.cmp(&left.alloc).then(left_ext.cmp(right_ext))
    });
    let actual: Vec<_> = snapshot
        .ext_stats()
        .iter()
        .map(|group| {
            (
                group.extension(),
                Totals {
                    size: group.total_size(),
                    alloc: group.total_alloc(),
                    files: group.count(),
                },
            )
        })
        .collect();
    assert_eq!(
        actual,
        expected
            .iter()
            .map(|(extension, total)| (extension.as_str(), *total))
            .collect::<Vec<_>>()
    );
    Ok(())
}

// Used only for post-scan assertions, never for planning or serving a read request.
fn physical(manifest: &Manifest, virtual_byte: u64, len: u64) -> TestResult<u64> {
    let cluster = u64::from(manifest.geometry.cluster_size);
    let extent = manifest
        .mft
        .extents
        .iter()
        .find(|extent| {
            virtual_byte >= extent.vcn * cluster
                && virtual_byte < (extent.vcn + extent.len) * cluster
        })
        .ok_or("oracle unmapped MFT byte")?;
    assert!(virtual_byte + len <= (extent.vcn + extent.len) * cluster);
    Ok(extent.lcn.ok_or("sparse MFT fixture")? * cluster + virtual_byte - extent.vcn * cluster)
}
fn deleted_records(fixture: &Fixture) -> TestResult<BTreeSet<u64>> {
    let manifest = fixture.manifest();
    let record_size = u64::from(manifest.geometry.file_record_size);
    let mut result = BTreeSet::new();
    for record in 0..manifest.mft.data_size / record_size {
        // Signatures and flags precede all protected sector trailers, so no fixup is needed.
        let offset = usize::try_from(physical(manifest, record * record_size, 24)?)?;
        let header = fixture
            .image()
            .get(offset..offset + 24)
            .ok_or("oracle header outside image")?;
        let flags = u16::from_le_bytes(header.get(22..24).ok_or("header flags")?.try_into()?);
        if header.get(..4) == Some(b"FILE".as_slice()) && flags & 1 == 0 {
            result.insert(record);
        }
    }
    Ok(result)
}
fn stats_and_coverage(
    name: &str,
    fixture: &Fixture,
    driven: &Driven,
    paths: &BTreeMap<String, NodeId>,
    oracle: &Oracle<'_>,
) -> TestResult {
    let manifest = fixture.manifest();
    let snapshot = &driven.output.snapshot;
    let stats = &driven.output.stats;
    assert_eq!(
        stats.records_total,
        manifest.mft.data_size / u64::from(manifest.geometry.file_record_size)
    );
    let deleted = deleted_records(fixture)?;
    assert_eq!(stats.deleted, u64::try_from(deleted.len())?);
    for raw in 0..u32::try_from(snapshot.len())? {
        if let Some(reference) = node(snapshot, NodeId::from_raw(raw))?.file_ref() {
            assert!(!deleted.contains(&reference.record()));
        }
    }
    for entry in &manifest.deleted {
        assert!(!paths.contains_key(&entry.path));
        assert!(deleted.contains(&entry.record));
    }
    let failures = [
        ("corrupt_records", stats.corrupt_records),
        ("bad_fixup", stats.bad_fixup),
        ("invalid_fixup_metadata", stats.invalid_fixup_metadata),
        ("invalid_signature", stats.invalid_signature),
        ("corrupt_headers", stats.corrupt_headers),
        ("corrupt_attributes", stats.corrupt_attributes),
        ("type_storage_mismatches", stats.type_storage_mismatches),
        ("corrupt_values", stats.corrupt_values),
        ("corrupt_runlists", stats.corrupt_runlists),
        ("out_of_volume_extents", stats.out_of_volume_extents),
        ("invalid_attribute_lists", stats.invalid_attribute_lists),
        ("invalid_file_refs", stats.invalid_file_refs),
        ("invalid_utf16", stats.invalid_utf16),
        ("rejected_extensions", stats.rejected_extensions),
        ("corrupt_streams", stats.corrupt_streams),
        ("baad", stats.baad),
        ("orphans", stats.orphans),
        ("cycles", stats.cycle_nodes),
    ];
    for (counter, value) in failures {
        assert_eq!(value, 0, "{name}: {counter}");
    }
    assert_eq!(
        stats.records_in_use,
        stats.base_records + stats.extension_records
    );
    assert_eq!(
        stats.records_total,
        stats.records_in_use + stats.deleted + stats.unused
    );
    let bases: BTreeSet<_> = oracle
        .rows
        .values()
        .filter_map(|row| row.reference.map(|reference| reference.0))
        .collect();
    assert_eq!(stats.base_records, u64::try_from(bases.len())?);
    assert_eq!(
        stats.hardlink_duplicates,
        u64::try_from(oracle.rows.values().filter(|row| row.duplicate).count())?
    );
    assert_eq!(
        stats.metafiles,
        u64::try_from(
            manifest
                .metafiles
                .iter()
                .map(|entry| entry.record)
                .collect::<BTreeSet<_>>()
                .len()
        )?
    );
    coverage(name, fixture, driven, paths, oracle, &deleted)?;
    Ok(())
}

fn coverage(
    name: &str,
    fixture: &Fixture,
    driven: &Driven,
    paths: &BTreeMap<String, NodeId>,
    oracle: &Oracle<'_>,
    deleted: &BTreeSet<u64>,
) -> TestResult {
    let manifest = fixture.manifest();
    let snapshot = &driven.output.snapshot;
    let stats = &driven.output.stats;
    let geometry = snapshot.volume().geometry();
    assert_eq!(
        (
            geometry.bytes_per_sector(),
            geometry.cluster_size(),
            geometry.file_record_size(),
            geometry.index_record_size()
        ),
        (
            manifest.geometry.bytes_per_sector,
            manifest.geometry.cluster_size,
            manifest.geometry.file_record_size,
            manifest.geometry.index_record_size
        )
    );
    assert_eq!(
        (
            geometry.total_clusters(),
            geometry.mft_lcn(),
            geometry.mftmirr_lcn()
        ),
        (
            manifest.geometry.total_clusters,
            manifest.geometry.mft_lcn,
            manifest.geometry.mftmirr_lcn
        )
    );
    match name {
        "basic-4k" => {
            assert!(stats.deleted >= 1);
            assert!(!paths.contains_key("/deleted.txt"));
            assert!(
                oracle
                    .rows
                    .values()
                    .all(|row| String::from_utf16_lossy(&row.name) != "LONGFI~1.TXT")
            );
            assert!(
                (0..u32::try_from(snapshot.len())?)
                    .all(|raw| snapshot.name(NodeId::from_raw(raw)) != Some("LONGFI~1.TXT"))
            );
            let links: Vec<_> = oracle
                .rows
                .values()
                .filter(|row| row.reference.is_some_and(|reference| reference.0 == 270))
                .collect();
            assert_eq!(links.len(), 3);
            assert_eq!(links.iter().filter(|row| !row.duplicate).count(), 1);
            let parents: BTreeSet<_> = links.iter().map(|row| &row.parent).collect();
            assert_eq!(
                parents.len(),
                2,
                "same-parent and cross-directory links must both be exercised"
            );
            assert!(
                manifest
                    .entries
                    .iter()
                    .any(|entry| entry.path.matches('/').count() >= 41)
            );
            assert!(manifest.entries.iter().any(|entry| !entry.path.is_ascii()));
            assert!(paths.contains_key("/日本語.txt"));
            assert!(paths.contains_key("/emoji-😀.txt"));
        }
        "frag-512" => {
            assert!(manifest.mft.has_attribute_list && manifest.mft.extents.len() > 1);
            assert!(
                deleted.contains(&4095),
                "cross-extent record must be a deleted FILE record"
            );
            let start = 4095u64 * u64::from(manifest.geometry.file_record_size);
            let end = start + u64::from(manifest.geometry.file_record_size);
            let boundary = manifest
                .mft
                .extents
                .iter()
                .map(|extent| extent.vcn * u64::from(manifest.geometry.cluster_size))
                .find(|boundary| start < *boundary && *boundary < end)
                .ok_or("record 4095 must straddle extents")?;
            let requested_ranges: Vec<_> = driven
                .reads
                .iter()
                .map(|request| {
                    Ok((
                        request.offset,
                        request
                            .offset
                            .checked_add(u64::try_from(request.len)?)
                            .ok_or("request end overflow")?,
                    ))
                })
                .collect::<TestResult<_>>()?;
            let mut physical_pieces = Vec::new();
            for (from, to) in [(start, boundary), (boundary, end)] {
                let offset = physical(manifest, from, to - from)?;
                assert!(
                    requested_ranges
                        .iter()
                        .any(|(start, end)| *start <= offset && *end >= offset + to - from),
                    "both halves of record 4095 must be requested"
                );
                physical_pieces.push((offset, to - from));
            }
            let first = physical_pieces.first().ok_or("first extent")?;
            let second = physical_pieces.get(1).ok_or("second extent")?;
            assert_ne!(
                first.0 + first.1,
                second.0,
                "the record must cross a physical discontinuity"
            );
        }
        "4kn-64k" => {
            assert_eq!(geometry.bytes_per_sector(), 4096);
            assert_eq!(geometry.cluster_size(), 65_536);
        }
        _ => return Err("unknown fixture".into()),
    }
    Ok(())
}

fn same_snapshot(left: &Snapshot, right: &Snapshot) -> TestResult {
    assert_eq!(left.len(), right.len());
    assert_eq!(left.is_empty(), right.is_empty());
    assert_eq!(left.volume().geometry(), right.volume().geometry());
    assert_eq!(left.volume().context(), right.volume().context());
    for raw in 0..u32::try_from(left.len())? {
        let id = NodeId::from_raw(raw);
        let a = node(left, id)?;
        let b = node(right, id)?;
        assert_eq!(
            (
                a.id(),
                a.parent(),
                a.child_start(),
                a.child_len(),
                a.name_off(),
                a.name_len()
            ),
            (
                b.id(),
                b.parent(),
                b.child_start(),
                b.child_len(),
                b.name_off(),
                b.name_len()
            ),
            "node {raw}: structure"
        );
        assert_eq!(
            (
                a.size(),
                a.ads_size(),
                a.alloc(),
                a.total_size(),
                a.total_alloc(),
                a.file_count()
            ),
            (
                b.size(),
                b.ads_size(),
                b.alloc(),
                b.total_size(),
                b.total_alloc(),
                b.file_count()
            ),
            "node {raw}: accounting"
        );
        assert_eq!(
            (
                a.file_ref(),
                a.attrs(),
                a.mtime(),
                a.nlink(),
                a.reparse_tag(),
                a.flags()
            ),
            (
                b.file_ref(),
                b.attrs(),
                b.mtime(),
                b.nlink(),
                b.reparse_tag(),
                b.flags()
            ),
            "node {raw}: metadata"
        );
        assert_eq!(left.name(id), right.name(id));
        assert_eq!(left.path(id)?, right.path(id)?);
        assert_eq!(left.children(id), right.children(id));
    }
    assert_eq!(left.top_files(), right.top_files());
    let stats = |snapshot: &Snapshot| {
        snapshot
            .ext_stats()
            .iter()
            .map(|group| {
                (
                    group.extension().to_owned(),
                    group.count(),
                    group.total_size(),
                    group.total_alloc(),
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(stats(left), stats(right));
    Ok(())
}

fn fixture_suite(name: &str, expected_root_alloc: u64) -> TestResult {
    let fixture = Fixture::load(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/ntfs")
            .join(format!("{name}.json")),
    )?;
    assert_eq!(fixture.manifest().schema_version, 3);
    let records_planned =
        fixture.manifest().mft.data_size / u64::from(fixture.manifest().geometry.file_record_size);
    let baseline = scan(
        fixture.image(),
        ScanConfig::default(),
        Arrival::Issued,
        records_planned,
    )?;
    let actual_paths = paths(&baseline.output.snapshot)?;
    let snapshot = &baseline.output.snapshot;
    let root = node(snapshot, NodeId::ROOT)?;
    assert_eq!(
        root.total_alloc(),
        expected_root_alloc,
        "{name}: root allocation"
    );
    assert!(
        root.total_alloc() <= snapshot.volume().geometry().total_bytes(),
        "{name}: root exceeds volume"
    );
    let badclus: Vec<_> = actual_paths
        .values()
        .filter_map(|&id| snapshot.node(id))
        .filter(|node| {
            node.file_ref()
                .is_some_and(|reference| reference.record() == 8)
        })
        .collect();
    assert_eq!(badclus.len(), 1, "{name}: $BadClus node count");
    assert_eq!(
        badclus.first().ok_or("$BadClus node")?.alloc(),
        0,
        "{name}: $BadClus allocation"
    );

    let allocation = MappedAllocation::from_reference(&ReferenceMft::from_fixture(&fixture)?)?;
    let oracle = Oracle::new(
        fixture.manifest(),
        &allocation,
        &baseline.output.snapshot,
        &actual_paths,
    )?;
    compare_rows(
        &oracle,
        fixture.manifest(),
        &baseline.output.snapshot,
        &actual_paths,
    )?;
    compare_indexes(&oracle, &baseline.output.snapshot, &actual_paths)?;
    stats_and_coverage(name, &fixture, &baseline, &actual_paths, &oracle)?;
    let configs = [
        (
            ScanConfig {
                chunk_size: 4096,
                ..ScanConfig::default()
            },
            Arrival::Issued,
        ),
        (
            ScanConfig {
                chunk_size: 4096,
                max_outstanding: 4,
                ..ScanConfig::default()
            },
            Arrival::Reversed,
        ),
    ];
    for (config, arrival) in configs {
        let driven = scan(fixture.image(), config, arrival, records_planned)?;
        if matches!(arrival, Arrival::Reversed) {
            assert!(
                driven.reversed_batches > 0,
                "the driver must actually reorder a multi-request batch"
            );
        }
        same_snapshot(&baseline.output.snapshot, &driven.output.snapshot)?;
        assert_eq!(baseline.output.stats, driven.output.stats);
        stats_and_coverage(name, &fixture, &driven, &actual_paths, &oracle)?;
    }
    Ok(())
}

#[test]
fn fixtures_basic_4k_public_oracle() -> TestResult {
    fixture_suite("basic-4k", 3_039_232)
}
#[test]
fn fixtures_fragmented_512_public_oracle() -> TestResult {
    fixture_suite("frag-512", 31_450_112)
}
#[test]
fn fixtures_native_4kn_64k_public_oracle() -> TestResult {
    fixture_suite("4kn-64k", 3_538_944)
}

#[test]
fn synthetic_progress_counts_every_slot_including_unused_records() -> TestResult {
    use adamite_fixtures::{SyntheticConfig, SyntheticVolume};
    let config = SyntheticConfig::reference(1_002)?;
    let slots = u64::from(config.slot_count);
    let volume = SyntheticVolume::new(config)?;
    let mut image = std::io::Cursor::new(Vec::new());
    volume.write(&mut image)?;
    let driven = scan(
        image.get_ref(),
        ScanConfig {
            chunk_size: 4096,
            max_outstanding: 4,
            ..ScanConfig::default()
        },
        Arrival::Reversed,
        slots,
    )?;
    assert!(driven.output.stats.unused > 0);
    Ok(())
}
