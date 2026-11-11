//! Query results compared with full sorting and independent file folds.

use super::{TestResult, entry, finish as build, root};
use crate::{
    graph::entry::EntryName,
    scan::ScanContext,
    snapshot::{
        FileFilter, FileKind, NodeFlags, NodeId, QueryError, Snapshot, SnapshotError,
        kind::EXTENSION_KINDS,
    },
};
use adamite_ntfs::record::reference::FileRef;
use proptest::prelude::*;
use std::{
    cmp::Reverse,
    collections::{BTreeMap, TryReserveError},
    error::Error,
};

const SUFFIXES: [&str; 9] = ["MP4", "mkv", "RS", "txt", "zip", "iso", "", "ÄBC", "äBC"];

fn generated_snapshot(files: &[(u16, u8, bool)]) -> Result<Snapshot, Box<dyn Error>> {
    let mut entries = vec![
        root(),
        entry(64, 5, "sub", true),
        entry(65, 64, "nested", true),
        entry(66, 5, "empty", true),
    ];
    for (index, (alloc, selector, duplicate)) in files.iter().enumerate() {
        let record = u64::try_from(index)?.checked_add(100).ok_or("record")?;
        let suffix = SUFFIXES
            .get(
                usize::from(*selector)
                    .checked_rem(SUFFIXES.len())
                    .ok_or("suffix count")?,
            )
            .ok_or("suffix")?;
        // Repeated names and allocation ties exercise reference and identifier tie breaks.
        let name = if suffix.is_empty() {
            "README".to_owned()
        } else {
            format!("same.{suffix}")
        };
        let parent = match selector % 3 {
            0 => 5,
            1 => 64,
            _ => 65,
        };
        let mut file = entry(record, parent, &name, false);
        file.alloc = u64::from(*alloc);
        file.size = u64::from(alloc % 17);
        if *duplicate {
            file.names.push(EntryName {
                parent: FileRef::from_raw((7u64 << 48) | 64),
                namespace: 1,
                name: format!("z-alias-{index}.pdf").encode_utf16().collect(),
            });
        }
        entries.push(file);
    }
    build(entries, &ScanContext::default())
}

fn scope(snapshot: &Snapshot, name: &str) -> Result<NodeId, Box<dyn Error>> {
    (0..u32::try_from(snapshot.len())?)
        .map(NodeId::from_raw)
        .find(|id| snapshot.name(*id) == Some(name))
        .ok_or_else(|| "scope missing".into())
}

fn primary_files_under(snapshot: &Snapshot, under: NodeId) -> Result<Vec<NodeId>, Box<dyn Error>> {
    let mut files = Vec::new();
    for raw in 0..u32::try_from(snapshot.len())? {
        let id = NodeId::from_raw(raw);
        let node = snapshot.node(id).ok_or("node")?;
        if node.is_dir()
            || node.flags().contains(NodeFlags::SYNTHETIC)
            || node.flags().contains(NodeFlags::HARDLINK_DUP)
        {
            continue;
        }
        let mut ancestor = id;
        loop {
            if ancestor == under {
                files.push(id);
                break;
            }
            if ancestor == NodeId::ROOT {
                break;
            }
            ancestor = snapshot.node(ancestor).ok_or("ancestor")?.parent();
        }
    }
    Ok(files)
}

fn full_sort(
    snapshot: &Snapshot,
    under: NodeId,
    mut matches: impl FnMut(&str) -> bool,
    n: usize,
) -> Result<Vec<NodeId>, Box<dyn Error>> {
    let mut files = Vec::new();
    for id in primary_files_under(snapshot, under)? {
        let node = snapshot.node(id).ok_or("file")?;
        let name = snapshot.name(id).ok_or("name")?;
        if matches(name) {
            files.push((
                Reverse(node.alloc()),
                name.encode_utf16().collect::<Vec<_>>(),
                node.file_ref()
                    .map(|reference| (reference.record(), reference.sequence())),
                id,
            ));
        }
    }
    files.sort_unstable();
    Ok(files.into_iter().take(n).map(|(_, _, _, id)| id).collect())
}

fn extension_key(name: &str) -> String {
    match name.rsplit_once('.') {
        Some((stem, suffix)) if !stem.is_empty() && !suffix.is_empty() => {
            suffix.to_ascii_lowercase()
        }
        _ => String::new(),
    }
}

fn check_totals(snapshot: &Snapshot, under: NodeId) -> TestResult {
    let mut expected = BTreeMap::<String, (u64, u64, u64)>::new();
    for id in primary_files_under(snapshot, under)? {
        let node = snapshot.node(id).ok_or("file")?;
        let key = extension_key(snapshot.name(id).ok_or("name")?);
        let totals = expected.entry(key).or_default();
        totals.0 = totals.0.checked_add(1).ok_or("count")?;
        totals.1 = totals.1.checked_add(node.size()).ok_or("size")?;
        totals.2 = totals.2.checked_add(node.alloc()).ok_or("alloc")?;
    }
    let mut expected: Vec<_> = expected.into_iter().collect();
    expected.sort_unstable_by(|left, right| {
        right.1.2.cmp(&left.1.2).then_with(|| left.0.cmp(&right.0))
    });
    let extensions = snapshot.extension_stats_under(under)?;
    let actual: Vec<_> = extensions
        .iter()
        .map(|group| {
            (
                group.extension().to_owned(),
                (group.count(), group.total_size(), group.total_alloc()),
            )
        })
        .collect();
    assert_eq!(actual, expected, "extension totals under {under:?}");
    let kinds = snapshot.kind_stats(under)?;
    let mut kind_totals = (0u64, 0u64, 0u64);
    let mut ext_totals = (0u64, 0u64, 0u64);
    for group in &extensions {
        ext_totals.0 = ext_totals.0.checked_add(group.count()).ok_or("count")?;
        ext_totals.1 = ext_totals.1.checked_add(group.total_size()).ok_or("size")?;
        ext_totals.2 = ext_totals
            .2
            .checked_add(group.total_alloc())
            .ok_or("alloc")?;
    }
    for group in &kinds {
        assert!(group.count() > 0, "empty kind emitted");
        kind_totals.0 = kind_totals.0.checked_add(group.count()).ok_or("count")?;
        kind_totals.1 = kind_totals
            .1
            .checked_add(group.total_size())
            .ok_or("size")?;
        kind_totals.2 = kind_totals
            .2
            .checked_add(group.total_alloc())
            .ok_or("alloc")?;
        let selected = snapshot.rank_files(under, &FileFilter::Kind(group.kind()), usize::MAX)?;
        assert_eq!(u64::try_from(selected.len())?, group.count());
        let (size, alloc) = selected
            .iter()
            .try_fold((0u64, 0u64), |(size, alloc), id| {
                let node = snapshot.node(*id).ok_or("kind file")?;
                Ok::<_, Box<dyn Error>>((
                    size.checked_add(node.size()).ok_or("size")?,
                    alloc.checked_add(node.alloc()).ok_or("alloc")?,
                ))
            })?;
        assert_eq!((size, alloc), (group.total_size(), group.total_alloc()));
    }
    assert_eq!(kind_totals, ext_totals, "kind totals under {under:?}");
    assert!(kinds.windows(2).all(|pair| matches!(pair, [left, right] if (Reverse(left.total_alloc()), left.kind()) < (Reverse(right.total_alloc()), right.kind()))));
    Ok(())
}

#[test]
fn kind_table_is_strictly_sorted_unique_and_parse_names_round_trip() -> TestResult {
    assert!(
        EXTENSION_KINDS
            .windows(2)
            .all(|pair| matches!(pair, [left, right] if left.0 < right.0))
    );
    assert!(
        EXTENSION_KINDS
            .iter()
            .all(|(key, _)| *key == key.to_ascii_lowercase())
    );
    for kind in FileKind::ALL {
        assert_eq!(kind.name().parse::<FileKind>()?, kind);
    }
    for invalid in ["", "Video", "diskimage", "unknown"] {
        assert!(matches!(
            invalid.parse::<FileKind>(),
            Err(QueryError::InvalidKind)
        ));
    }
    Ok(())
}

#[test]
fn filters_handle_one_dot_empty_suffix_ascii_folding_and_all_kinds() -> TestResult {
    let names = [
        "plain",
        ".hidden",
        "trailing.",
        "movie.MP4",
        "track.FLAC",
        "photo.HEIC",
        "bundle.7Z",
        "disk.VHDX",
        "paper.PDF",
        "main.RS",
        "lib.DLL",
        "unknown.zzz",
        "stream.ts",
        "text.ÄBC",
        "text.äBC",
        "many.dots.TAR.GZ",
        "other.hidden",
        "😀.mp4",
        "\u{e000}.mp4",
    ];
    let mut entries = vec![root()];
    for (record, name) in (100..).zip(names) {
        entries.push(entry(record, 5, name, false));
    }
    let snapshot = build(entries, &ScanContext::default())?;
    for (extensions, expected) in [
        (
            vec![".MP4", "mP4"],
            vec!["movie.MP4", "😀.mp4", "\u{e000}.mp4"],
        ),
        (vec![""], vec!["plain", ".hidden", "trailing."]),
        (vec!["."], vec!["plain", ".hidden", "trailing."]),
        (vec!["..mp4"], vec![]),
        (vec![".Äbc"], vec!["text.ÄBC"]),
        (
            vec![".mp4", "", "gz"],
            vec![
                "plain",
                ".hidden",
                "trailing.",
                "movie.MP4",
                "many.dots.TAR.GZ",
                "😀.mp4",
                "\u{e000}.mp4",
            ],
        ),
        (vec![], vec![]),
    ] {
        let filter = FileFilter::extensions(extensions)?;
        assert_eq!(
            snapshot.rank_files(NodeId::ROOT, &filter, usize::MAX)?,
            full_sort(
                &snapshot,
                NodeId::ROOT,
                |name| expected.contains(&name),
                usize::MAX
            )?
        );
    }
    for (kind, expected) in [
        (
            FileKind::Video,
            vec!["movie.MP4", "stream.ts", "😀.mp4", "\u{e000}.mp4"],
        ),
        (FileKind::Audio, vec!["track.FLAC"]),
        (FileKind::Image, vec!["photo.HEIC"]),
        (FileKind::Archive, vec!["bundle.7Z", "many.dots.TAR.GZ"]),
        (FileKind::DiskImage, vec!["disk.VHDX"]),
        (FileKind::Document, vec!["paper.PDF"]),
        (FileKind::Code, vec!["main.RS"]),
        (FileKind::Executable, vec!["lib.DLL"]),
        (
            FileKind::Other,
            vec![
                "plain",
                ".hidden",
                "trailing.",
                "unknown.zzz",
                "text.ÄBC",
                "text.äBC",
                "other.hidden",
            ],
        ),
    ] {
        assert_eq!(
            snapshot.rank_files(NodeId::ROOT, &FileFilter::Kind(kind), usize::MAX)?,
            full_sort(
                &snapshot,
                NodeId::ROOT,
                |name| expected.contains(&name),
                usize::MAX
            )?
        );
    }
    check_totals(&snapshot, NodeId::ROOT)
}

#[test]
fn filtered_ranking_finds_hits_beyond_the_stored_hundred() -> TestResult {
    let mut files = vec![(4096, 2, false); 150];
    files.extend(vec![(512, 0, true); 120]);
    files.extend(vec![(512, 1, true); 120]);
    let snapshot = generated_snapshot(&files)?;
    assert!(
        snapshot
            .top_files()
            .iter()
            .all(|id| snapshot.name(*id) == Some("same.RS"))
    );
    for under in [
        NodeId::ROOT,
        scope(&snapshot, "sub")?,
        scope(&snapshot, "nested")?,
    ] {
        for n in [0, 1, 99, 100, 101, 200, usize::MAX] {
            assert_eq!(
                snapshot.rank_files(under, &FileFilter::Kind(FileKind::Video), n)?,
                full_sort(
                    &snapshot,
                    under,
                    |name| name.ends_with(".MP4") || name.ends_with(".mkv"),
                    n
                )?
            );
        }
        check_totals(&snapshot, under)?;
    }
    assert_eq!(
        snapshot
            .rank_files(NodeId::ROOT, &FileFilter::Kind(FileKind::Video), usize::MAX)?
            .len(),
        240
    );
    assert!(
        snapshot
            .kind_stats(NodeId::ROOT)?
            .iter()
            .all(|group| group.kind() != FileKind::Document),
        "duplicate PDF aliases must not contribute"
    );
    for n in 0..=100 {
        assert_eq!(
            snapshot.rank_files(NodeId::ROOT, &FileFilter::All, n)?,
            snapshot.top_files().get(..n).ok_or("prefix")?
        );
    }
    Ok(())
}

#[test]
fn query_scopes_accept_files_and_reject_unknown_ids_even_with_zero_limit() -> TestResult {
    let snapshot = generated_snapshot(&[(0, 0, true), (512, 1, false)])?;
    for raw in 0..u32::try_from(snapshot.len())? {
        let id = NodeId::from_raw(raw);
        let node = snapshot.node(id).ok_or("node")?;
        if !node.is_dir() {
            let expected = if node.flags().contains(NodeFlags::HARDLINK_DUP) {
                vec![]
            } else {
                vec![id]
            };
            assert_eq!(
                snapshot.rank_files(id, &FileFilter::All, usize::MAX)?,
                expected
            );
        }
        check_totals(&snapshot, id)?;
    }
    assert_eq!(
        snapshot.extension_stats_under(NodeId::ROOT)?,
        snapshot.ext_stats()
    );
    for id in [
        NodeId::from_raw(u32::try_from(snapshot.len())?),
        NodeId::from_raw(u32::MAX),
    ] {
        for n in [0, 1] {
            assert!(
                matches!(snapshot.rank_files(id, &FileFilter::All, n), Err(QueryError::InvalidNode { node }) if node == id)
            );
        }
        assert!(
            matches!(snapshot.extension_stats_under(id), Err(QueryError::InvalidNode { node }) if node == id)
        );
        assert!(
            matches!(snapshot.kind_stats(id), Err(QueryError::InvalidNode { node }) if node == id)
        );
    }
    let empty = generated_snapshot(&[])?;
    assert!(empty.extension_stats_under(NodeId::ROOT)?.is_empty());
    assert!(empty.kind_stats(NodeId::ROOT)?.is_empty());
    Ok(())
}

#[test]
fn query_errors_preserve_allocation_and_snapshot_sources() -> TestResult {
    let source = Vec::<u8>::new()
        .try_reserve(usize::MAX)
        .err()
        .ok_or("reservation succeeded")?;
    let error = QueryError::from(source);
    assert!(
        error
            .source()
            .and_then(|source| source.downcast_ref::<TryReserveError>())
            .is_some()
    );
    assert!(matches!(error, QueryError::Allocation { .. }));
    let error = QueryError::from(SnapshotError::overflow("test"));
    assert!(
        error
            .source()
            .and_then(|source| source.downcast_ref::<SnapshotError>())
            .is_some()
    );
    assert!(matches!(error, QueryError::Snapshot { .. }));
    Ok(())
}

proptest! {
    #[test]
    fn subtree_queries_match_full_sort_and_brute_force_totals(
        files in prop::collection::vec((0u16..100, any::<u8>(), any::<bool>()), 0..200),
        n in 0usize..250,
    ) {
        let verify = || -> TestResult {
            let snapshot = generated_snapshot(&files)?;
            for under in [NodeId::ROOT, scope(&snapshot, "sub")?, scope(&snapshot, "nested")?] {
                assert_eq!(snapshot.rank_files(under, &FileFilter::All, n)?, full_sort(&snapshot, under, |_| true, n)?);
                assert_eq!(snapshot.rank_files(under, &FileFilter::extensions([".MP4", "mkv", ""])? , n)?, full_sort(&snapshot, under, |name| matches!(extension_key(name).as_str(), "mp4" | "mkv" | ""), n)?);
                assert_eq!(snapshot.rank_files(under, &FileFilter::Kind(FileKind::Video), n)?, full_sort(&snapshot, under, |name| matches!(extension_key(name).as_str(), "mp4" | "mkv"), n)?);
                check_totals(&snapshot, under)?;
            }
            let prefix = n.min(100).min(snapshot.top_files().len());
            assert_eq!(snapshot.rank_files(NodeId::ROOT, &FileFilter::All, n.min(100))?, snapshot.top_files().get(..prefix).ok_or("prefix")?);
            assert_eq!(snapshot.extension_stats_under(NodeId::ROOT)?, snapshot.ext_stats());
            Ok(())
        };
        verify().map_err(|error| TestCaseError::fail(error.to_string()))?;
    }
}
