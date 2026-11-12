//! Path matching, direct-child selectors and round trips over generated trees.

use super::{TestResult, build, entry, root};
use crate::{
    scan::ScanContext,
    snapshot::{NodeId, QueryError, Snapshot},
};
use proptest::prelude::*;
use std::error::Error;

fn path_snapshot() -> Result<Snapshot, Box<dyn Error>> {
    build(
        vec![
            root(),
            entry(64, 5, "Twin", true),
            entry(65, 5, "twin", true),
            entry(66, 5, "same", true),
            entry(67, 5, "same", true),
            entry(68, 5, "İ", true),
            entry(69, 5, ".", true),
            entry(70, 5, "..", true),
            entry(71, 5, "bigdir", true),
            entry(72, 71, "f000", false),
            entry(73, 5, "tiny.txt", false),
            entry(74, 5, "#bad", true),
            entry(75, 5, "line\n\t\u{1b}.TX\nT", true),
            entry(76, 75, "file\n.tx\nt", false),
            entry(77, 5, "#123", false),
        ],
        &ScanContext::default(),
    )
}

#[test]
fn paths_prefer_exact_names_then_unicode_lowercase_and_skip_empty_components() -> TestResult {
    let snapshot = path_snapshot()?;
    for (path, raw) in [
        ("/Twin", 1),
        ("/twin", 2),
        ("/i\u{307}", 5),
        ("/İ", 5),
        ("/.", 6),
        ("/..", 7),
        ("bigdir", 8),
        ("/bigdir/", 8),
        ("//BIGDIR//", 8),
        ("\\bigdir\\", 8),
        ("/\\BiGdIr\\/", 8),
        ("\\BIGDIR\\F000", 9),
        ("/tiny.txt", 10),
        ("/tiny.txt/", 10),
        ("/line\n\t\u{1b}.TX\nT/file\n.tx\nt", 13),
    ] {
        assert_eq!(
            snapshot.resolve_path(path)?,
            NodeId::from_raw(raw),
            "{path:?}"
        );
    }
    for path in ["", "/", "\\", "////\\\\"] {
        assert_eq!(snapshot.resolve_path(path)?, NodeId::ROOT, "{path:?}");
    }
    Ok(())
}

#[test]
fn id_selectors_resolve_only_direct_children_and_require_ascii_digits() -> TestResult {
    let snapshot = path_snapshot()?;
    for (path, raw) in [
        ("/#1", 1),
        ("/#2", 2),
        ("/#3", 3),
        ("/#4", 4),
        ("/#0008/", 8),
        ("/bigdir/#9", 9),
        ("/#8/#9", 9),
        ("/#11", 11),
        ("/#14", 14),
    ] {
        assert_eq!(
            snapshot.resolve_path(path)?,
            NodeId::from_raw(raw),
            "{path:?}"
        );
    }
    for (path, missing) in [
        ("/missing", "missing"),
        ("/bigdir/missing", "missing"),
        ("/Twin/..", ".."),
        ("/#4294967295", "#4294967295"),
        ("/#0", "#0"),
        ("/#bad", "#bad"),
        ("/#", "#"),
        ("/#-1", "#-1"),
        ("/#+1", "#+1"),
        ("/#4294967296", "#4294967296"),
        ("/#９", "#９"),
        ("/#1a", "#1a"),
        ("/#123", "#123"),
        ("/#9", "#9"),
        ("/bigdir/#8", "#8"),
    ] {
        let error = snapshot
            .resolve_path(path)
            .err()
            .ok_or("missing path resolved")?;
        assert!(
            matches!(error, QueryError::PathNotFound { path: ref supplied, ref component }
                if supplied == path && component == missing),
            "{path:?}: {error:?}"
        );
    }
    Ok(())
}

#[test]
fn ambiguous_components_return_all_candidates_in_stored_child_order() -> TestResult {
    let snapshot = path_snapshot()?;
    for (path, component, candidates) in [
        ("/TWIN", "TWIN", [NodeId::from_raw(1), NodeId::from_raw(2)]),
        ("/same", "same", [NodeId::from_raw(3), NodeId::from_raw(4)]),
        (
            "/TWIN/child",
            "TWIN",
            [NodeId::from_raw(1), NodeId::from_raw(2)],
        ),
    ] {
        let error = snapshot
            .resolve_path(path)
            .err()
            .ok_or("ambiguous path resolved")?;
        assert!(
            matches!(error, QueryError::AmbiguousPath { path: ref supplied, component: ref part, candidates: ref ids }
                if supplied == path && part == component && ids == &candidates),
            "{path:?}: {error:?}"
        );
    }
    Ok(())
}

#[test]
fn descending_through_a_file_reports_the_file_identifier() -> TestResult {
    let snapshot = path_snapshot()?;
    for (path, raw) in [
        ("/tiny.txt/child", 10),
        ("/TINY.TXT//child", 10),
        ("/#10/#1", 10),
        ("/bigdir/#9/child", 9),
    ] {
        assert!(
            matches!(snapshot.resolve_path(path), Err(QueryError::NotADirectory { node })
                if node == NodeId::from_raw(raw)),
            "{path:?}"
        );
    }
    Ok(())
}

#[test]
fn synthetic_directories_follow_the_same_name_rules() -> TestResult {
    let snapshot = build(
        vec![
            root(),
            entry(0, 5, "$MFT", false),
            entry(64, 999, "lost", false),
        ],
        &ScanContext::default(),
    )?;
    for (path, canonical) in [
        ("/<ntfs METADATA>", "/<NTFS metadata>"),
        ("/<ORPHANED>/lost", "/<orphaned>/lost"),
    ] {
        let id = snapshot.resolve_path(path)?;
        assert_eq!(snapshot.path(id)?.as_deref(), Some(canonical), "{path:?}");
    }
    Ok(())
}

proptest! {
    #[test]
    fn unique_name_paths_round_trip(
        directories in prop::collection::vec(any::<u8>(), 0..30),
        files in prop::collection::vec(any::<u8>(), 0..60),
    ) {
        let verify = || -> TestResult {
            let mut entries = vec![root()];
            let mut parents = vec![5];
            for (index, selector) in directories.iter().enumerate() {
                let record = u64::try_from(index)?.checked_add(64).ok_or("record")?;
                let parent_index = usize::from(*selector).checked_rem(parents.len()).ok_or("parent count")?;
                let parent = *parents.get(parent_index).ok_or("parent")?;
                entries.push(entry(record, parent, &format!("目录İ-{index}"), true));
                parents.push(record);
            }
            for (index, selector) in files.iter().enumerate() {
                let record = u64::try_from(index)?.checked_add(1000).ok_or("record")?;
                let parent_index = usize::from(*selector).checked_rem(parents.len()).ok_or("parent count")?;
                let parent = *parents.get(parent_index).ok_or("parent")?;
                entries.push(entry(record, parent, &format!("文件😀-{index}"), false));
            }
            let snapshot = build(entries, &ScanContext::default())?;
            for raw in 0..u32::try_from(snapshot.len())? {
                let id = NodeId::from_raw(raw);
                let path = snapshot.path(id)?.ok_or("path")?;
                assert_eq!(snapshot.resolve_path(&path)?, id, "{path:?}");
            }
            Ok(())
        };
        verify().map_err(|error| TestCaseError::fail(error.to_string()))?;
    }
}
