//! Name matching, pre-order traversal and paging over generated snapshot trees.

use super::{TestResult, entry, finish, root};
use crate::{
    graph::entry::{EntryName, EntryRecord},
    scan::ScanContext,
    snapshot::{NamePattern, NodeFlags, NodeId, QueryError, Snapshot},
};
use adamite_ntfs::record::reference::FileRef;
use proptest::prelude::*;
use std::error::Error;

fn recursive_walk(snapshot: &Snapshot, under: NodeId, nodes: &mut Vec<NodeId>) -> TestResult {
    for child in snapshot.children(under).ok_or("children")? {
        nodes.push(*child);
        recursive_walk(snapshot, *child, nodes)?;
    }
    Ok(())
}

fn named(snapshot: &Snapshot, name: &str) -> Result<NodeId, Box<dyn Error>> {
    (0..u32::try_from(snapshot.len())?)
        .map(NodeId::from_raw)
        .find(|id| snapshot.name(*id) == Some(name))
        .ok_or_else(|| "node not found".into())
}

fn entries_with_special_nodes() -> Vec<EntryRecord> {
    let mut linked = entry(100, 5, "primary.iso", false);
    linked.names.push(EntryName {
        parent: FileRef::from_raw((7u64 << 48) | 5),
        namespace: 1,
        name: "secondary.iso".encode_utf16().collect(),
    });
    vec![
        root(),
        entry(0, 5, "$MFT", false),
        entry(64, 5, "SteamLibrary", true),
        entry(65, 64, "child.iso", false),
        entry(66, 999_999, "lost.iso", false),
        linked,
    ]
}

fn generated_snapshot(
    directories: &[u8],
    files: &[(u8, u16, bool)],
) -> Result<Snapshot, Box<dyn Error>> {
    let mut entries = entries_with_special_nodes();
    let mut parents = vec![5, 64];
    for (index, selector) in directories.iter().enumerate() {
        let record = u64::try_from(index)?.checked_add(200).ok_or("record")?;
        let parent = *parents
            .get(
                usize::from(*selector)
                    .checked_rem(parents.len())
                    .ok_or("parent count")?,
            )
            .ok_or("parent")?;
        entries.push(entry(record, parent, &format!("dir-{index}"), true));
        parents.push(record);
    }
    for (index, (selector, alloc, duplicate)) in files.iter().enumerate() {
        let record = u64::try_from(index)?.checked_add(10_000).ok_or("record")?;
        let parent = *parents
            .get(
                usize::from(*selector)
                    .checked_rem(parents.len())
                    .ok_or("parent count")?,
            )
            .ok_or("parent")?;
        let suffix = if selector % 2 == 0 { "ISO" } else { "txt" };
        let mut file = entry(record, parent, &format!("file-{index}.{suffix}"), false);
        file.alloc = u64::from(*alloc);
        if *duplicate {
            file.names.push(EntryName {
                parent: FileRef::from_raw((7u64 << 48) | 64),
                namespace: 1,
                name: format!("alias-{index}.{suffix}").encode_utf16().collect(),
            });
        }
        entries.push(file);
    }
    finish(entries, &ScanContext::default())
}

#[test]
fn name_patterns_use_substrings_case_insensitivity_and_whole_name_globs() -> TestResult {
    for (input, matching, missing) in [
        (
            "steam",
            vec!["SteamLibrary", "mySTEAMgames", "steam"],
            vec!["stream", "library"],
        ),
        (
            "*.ISO",
            vec!["disc.iso", "BACKUP.ISO", "nested/disc.iso"],
            vec!["disc.iso.bak", "iso", "iso-disc"],
        ),
        ("[ab]*", vec!["Alpha", "beta", "B"], vec!["cat", "xAlpha"]),
        ("{x,y}*", vec!["xray", "Yard", "y"], vec!["zebra", "axray"]),
        ("a?c", vec!["abc", "AXC"], vec!["ac", "abbc", "xabc"]),
        (
            r"file\?.txt",
            vec!["file?.txt"],
            vec!["file1.txt", "xfile?.txt"],
        ),
        (
            r"file\*.txt",
            vec!["file*.txt"],
            vec!["file1.txt", "fileAA.txt"],
        ),
        (
            r"left\right",
            vec!["LEFTRIGHT", "xleftrightx"],
            vec![r"left\right", "left-right"],
        ),
        ("", vec!["", "anything", "folder/name"], vec![]),
    ] {
        let pattern = NamePattern::glob(input)?;
        assert_eq!(pattern.as_str(), input);
        for name in matching {
            assert!(pattern.matches(name), "{input:?} should match {name:?}");
        }
        for name in missing {
            assert!(!pattern.matches(name), "{input:?} should exclude {name:?}");
        }
    }
    Ok(())
}

#[test]
fn name_pattern_byte_limit_and_invalid_globs_preserve_errors() -> TestResult {
    for input in ["a".repeat(256), "é".repeat(128), "?".repeat(256)] {
        assert_eq!(NamePattern::glob(&input)?.as_str(), input);
    }
    for input in [
        "a".repeat(257),
        format!("{}a", "é".repeat(128)),
        format!("[{}", "a".repeat(256)),
    ] {
        let error = NamePattern::glob(&input)
            .err()
            .ok_or("oversized pattern accepted")?;
        assert!(matches!(error, QueryError::PatternTooLong { limit: 256 }));
        assert!(error.to_string().contains("256"));
    }
    for input in ["[", "[z-a]", "{x,y", "*.iso\\"] {
        let error = NamePattern::glob(input)
            .err()
            .ok_or("invalid glob accepted")?;
        let source = error
            .source()
            .and_then(|source| source.downcast_ref::<globset::Error>())
            .ok_or("glob source lost")?;
        assert_eq!(source.glob(), Some(input));
        assert!(matches!(error, QueryError::InvalidGlob { .. }));
    }
    Ok(())
}

#[test]
fn search_includes_synthetic_directories_and_duplicate_links_but_excludes_scope() -> TestResult {
    let snapshot = finish(entries_with_special_nodes(), &ScanContext::default())?;
    let all = NamePattern::glob("*")?;
    let mut expected = Vec::new();
    recursive_walk(&snapshot, NodeId::ROOT, &mut expected)?;
    let page = snapshot.search(NodeId::ROOT, &all, 0, usize::MAX)?;
    assert_eq!(page.items(), expected);
    assert_eq!(
        page.total_matches(),
        u64::try_from(snapshot.len().checked_sub(1).ok_or("root")?)?
    );
    assert_eq!(page.next_offset(), None);
    assert!(!page.items().contains(&NodeId::ROOT));
    let metadata = named(&snapshot, "<NTFS metadata>")?;
    let orphaned = named(&snapshot, "<orphaned>")?;
    let duplicate = named(&snapshot, "secondary.iso")?;
    for id in [metadata, orphaned, duplicate] {
        assert!(page.items().contains(&id));
    }
    assert!(
        snapshot
            .node(metadata)
            .ok_or("metadata")?
            .flags()
            .contains(NodeFlags::SYNTHETIC)
    );
    assert!(
        snapshot
            .node(duplicate)
            .ok_or("duplicate")?
            .flags()
            .contains(NodeFlags::HARDLINK_DUP)
    );
    let steam = NamePattern::glob("steam")?;
    assert_eq!(
        snapshot.search(NodeId::ROOT, &steam, 0, 10)?.items(),
        [named(&snapshot, "SteamLibrary")?]
    );
    let under = named(&snapshot, "SteamLibrary")?;
    assert!(snapshot.search(under, &steam, 0, 10)?.items().is_empty());
    assert_eq!(
        snapshot.search(under, &all, 0, 10)?.items(),
        [named(&snapshot, "child.iso")?]
    );
    assert_eq!(
        snapshot.search(metadata, &all, 0, 10)?.items(),
        [named(&snapshot, "$MFT")?]
    );
    let iso = snapshot.search(NodeId::ROOT, &NamePattern::glob("*.ISO")?, 0, usize::MAX)?;
    assert_eq!(iso.total_matches(), 4);
    assert!(iso.items().contains(&duplicate));
    Ok(())
}

fn check_paging(
    snapshot: &Snapshot,
    under: NodeId,
    pattern: &NamePattern,
    limit: usize,
) -> TestResult {
    let all = snapshot.search(under, pattern, 0, usize::MAX)?;
    let mut items = Vec::new();
    let mut offset = 0;
    loop {
        let page = snapshot.search(under, pattern, offset, limit)?;
        assert_eq!(page.total_matches(), all.total_matches());
        assert!(page.items().len() <= limit);
        items.extend_from_slice(page.items());
        if let Some(next) = page.next_offset() {
            assert_eq!(
                next,
                offset
                    .checked_add(page.items().len())
                    .ok_or("next offset")?
            );
            assert!(next > offset);
            offset = next;
        } else {
            assert_eq!(u64::try_from(items.len())?, all.total_matches());
            break;
        }
    }
    assert_eq!(items, all.items());
    Ok(())
}

#[test]
fn search_paging_offsets_zero_limit_file_scope_and_invalid_scope() -> TestResult {
    let snapshot = generated_snapshot(&[0, 1, 2], &[(0, 512, true), (1, 2, false)])?;
    let pattern = NamePattern::glob("*.iso")?;
    for limit in [1, 2, 3, 100, usize::MAX] {
        check_paging(&snapshot, NodeId::ROOT, &pattern, limit)?;
    }
    let all = snapshot.search(NodeId::ROOT, &pattern, 0, usize::MAX)?;
    let count = usize::try_from(all.total_matches())?;
    for offset in [
        0,
        1,
        count,
        count.checked_add(1).ok_or("past end")?,
        usize::MAX,
    ] {
        let page = snapshot.search(NodeId::ROOT, &pattern, offset, usize::MAX)?;
        assert_eq!(page.items(), all.items().get(offset..).unwrap_or(&[]));
        assert_eq!(page.total_matches(), all.total_matches());
        assert_eq!(page.next_offset(), None);
        let count_only = snapshot.search(NodeId::ROOT, &pattern, offset, 0)?;
        assert!(count_only.items().is_empty());
        assert_eq!(count_only.total_matches(), all.total_matches());
        assert_eq!(count_only.next_offset(), (offset < count).then_some(offset));
    }
    let file = named(&snapshot, "primary.iso")?;
    let empty = snapshot.search(file, &pattern, 0, 1)?;
    assert!(empty.items().is_empty());
    assert_eq!(empty.total_matches(), 0);
    assert_eq!(empty.next_offset(), None);
    let empty = finish(vec![root()], &ScanContext::default())?;
    check_paging(&empty, NodeId::ROOT, &NamePattern::glob("*")?, 1)?;
    for limit in [0, 1] {
        let invalid = NodeId::from_raw(u32::MAX);
        assert!(
            matches!(snapshot.search(invalid, &pattern, 0, limit), Err(QueryError::InvalidNode { node }) if node == invalid)
        );
    }
    Ok(())
}

#[test]
fn search_traverses_ten_thousand_deep_subtree_iteratively() -> TestResult {
    let mut entries = vec![root()];
    let mut parent = 5;
    for record in 64..10_064 {
        entries.push(entry(record, parent, "d", true));
        parent = record;
    }
    entries.push(entry(10_064, parent, "needle.iso", false));
    let snapshot = finish(entries, &ScanContext::default())?;
    let page = snapshot.search(NodeId::ROOT, &NamePattern::glob("*")?, 0, 2)?;
    assert_eq!(page.items(), [NodeId::from_raw(1), NodeId::from_raw(2)]);
    assert_eq!(page.total_matches(), 10_001);
    assert_eq!(page.next_offset(), Some(2));
    let page = snapshot.search(NodeId::from_raw(1), &NamePattern::glob("needle")?, 0, 1)?;
    assert_eq!(page.items(), [NodeId::from_raw(10_001)]);
    assert_eq!(page.total_matches(), 1);
    assert_eq!(page.next_offset(), None);
    Ok(())
}

proptest! {
    #[test]
    fn search_preorder_and_pages_match_recursive_walk(
        directories in prop::collection::vec(any::<u8>(), 0..12),
        files in prop::collection::vec((any::<u8>(), 0u16..4096, any::<bool>()), 0..50),
        selector in 0u8..4,
        scope_selector in any::<u8>(),
        limit in 1usize..64,
        offset in 0usize..150,
    ) {
        let verify = || -> TestResult {
            let snapshot = generated_snapshot(&directories, &files)?;
            let mut preorder = Vec::new();
            recursive_walk(&snapshot, NodeId::ROOT, &mut preorder)?;
            assert_eq!(snapshot.search(NodeId::ROOT, &NamePattern::glob("*")?, 0, usize::MAX)?.items(), preorder);
            let under = NodeId::from_raw(u32::from(scope_selector).checked_rem(u32::try_from(snapshot.len())?).ok_or("node count")?);
            let input = match selector { 0 => "*", 1 => "*.ISO", 2 => "dir-", _ => "no-match" };
            let pattern = NamePattern::glob(input)?;
            let mut expected = Vec::new();
            recursive_walk(&snapshot, under, &mut expected)?;
            expected.retain(|id| {
                let name = snapshot.name(*id).unwrap_or_default();
                match selector { 0 => true, 1 => name.to_ascii_lowercase().ends_with(".iso"), 2 => name.contains("dir-"), _ => false }
            });
            let all = snapshot.search(under, &pattern, 0, usize::MAX)?;
            assert_eq!(all.items(), expected);
            assert_eq!(all.total_matches(), u64::try_from(expected.len())?);
            assert_eq!(all.next_offset(), None);
            let page = snapshot.search(under, &pattern, offset, limit)?;
            let expected_page: Vec<_> = expected.iter().skip(offset).take(limit).copied().collect();
            assert_eq!(page.items(), expected_page);
            assert_eq!(page.total_matches(), all.total_matches());
            let next = offset.checked_add(page.items().len()).ok_or("next offset")?;
            assert_eq!(page.next_offset(), (next < expected.len()).then_some(next));
            check_paging(&snapshot, under, &pattern, limit)?;
            check_paging(&snapshot, NodeId::ROOT, &NamePattern::glob("*")?, limit)?;
            Ok(())
        };
        verify().map_err(|error| TestCaseError::fail(error.to_string()))?;
    }
}
