//! Budget, threshold, accounting and traversal properties of tree summaries.

use super::{TestResult, entry, finish, root};
use crate::{
    scan::ScanContext,
    snapshot::{NodeId, QueryError, Snapshot, Summary, SummaryRow},
};
use proptest::prelude::*;
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
};

struct SummaryTree;

impl SummaryTree {
    fn build(spec: &[(u16, u8, bool)]) -> Result<Snapshot, Box<dyn Error>> {
        let mut root = root();
        root.alloc = 0;
        root.size = 0;
        let mut entries = vec![root];
        let mut directories = vec![5u64];
        for (index, &(alloc, selector, directory)) in spec.iter().enumerate() {
            let record = u64::try_from(index)?.checked_add(64).ok_or("record")?;
            let parent = *directories
                .get(
                    usize::from(selector)
                        .checked_rem(directories.len())
                        .ok_or("directory count")?,
                )
                .ok_or("parent")?;
            let mut item = entry(record, parent, &format!("n-{index}"), directory);
            item.alloc = u64::from(alloc);
            item.size = u64::from(alloc).checked_add(3).ok_or("size")?;
            entries.push(item);
            if directory {
                directories.push(record);
            }
        }
        finish(entries, &ScanContext::default())
    }

    fn verify(
        snapshot: &Snapshot,
        root: NodeId,
        summary: &Summary,
        budget: usize,
        bp: u32,
    ) -> TestResult {
        assert!(!summary.rows.is_empty());
        assert!(summary.rows.len() <= budget);
        assert_eq!(
            summary.rows.first(),
            Some(&SummaryRow::Node { id: root, depth: 0 })
        );
        let mut seen = BTreeSet::new();
        let mut ancestors = Vec::new();
        let mut direct = BTreeMap::<NodeId, Vec<NodeId>>::new();
        let mut collapsed = BTreeSet::new();
        for row in &summary.rows {
            match *row {
                SummaryRow::Node { id, depth } => {
                    assert!(seen.insert(id), "duplicate {id:?}");
                    assert_eq!(ancestors.len().min(depth), depth, "depth jump");
                    ancestors.truncate(depth);
                    if let Some(parent) = ancestors.last().copied() {
                        assert_eq!(snapshot.node(id).ok_or("node")?.parent(), parent);
                        let denominator = snapshot.node(root).ok_or("root")?.total_alloc();
                        let alloc = snapshot.node(id).ok_or("node")?.total_alloc();
                        if denominator != 0 {
                            assert!(
                                u128::from(alloc).checked_mul(10_000).ok_or("share")?
                                    >= u128::from(denominator)
                                        .checked_mul(u128::from(bp))
                                        .ok_or("threshold")?
                            );
                        }
                        assert!(
                            !collapsed.contains(&parent),
                            "node after parent's collapsed suffix"
                        );
                        direct.entry(parent).or_default().push(id);
                    }
                    ancestors.push(id);
                }
                SummaryRow::Collapsed {
                    parent,
                    depth,
                    count,
                    alloc,
                    size,
                } => {
                    assert!(collapsed.insert(parent), "repeated collapsed suffix");
                    assert_eq!(
                        ancestors.get(depth.checked_sub(1).ok_or("depth")?),
                        Some(&parent)
                    );
                    ancestors.truncate(depth);
                    let children = snapshot.children(parent).ok_or("children")?;
                    let selected = direct.get(&parent).map_or(0, Vec::len);
                    let tail = children.get(selected..).ok_or("suffix")?;
                    assert_eq!(count, tail.len());
                    assert!(count > 0);
                    let mut expected_alloc = 0u64;
                    let mut expected_size = 0u64;
                    for &id in tail {
                        let node = snapshot.node(id).ok_or("child")?;
                        expected_alloc = expected_alloc
                            .checked_add(node.total_alloc())
                            .ok_or("alloc")?;
                        expected_size =
                            expected_size.checked_add(node.total_size()).ok_or("size")?;
                    }
                    assert_eq!((alloc, size), (expected_alloc, expected_size));
                    if !summary.truncated {
                        let first = snapshot.node(*tail.first().ok_or("first")?).ok_or("node")?;
                        let denominator = snapshot.node(root).ok_or("root")?.total_alloc();
                        assert!(denominator != 0);
                        assert!(
                            u128::from(first.total_alloc()) * 10_000
                                < u128::from(denominator)
                                    .checked_mul(u128::from(bp))
                                    .ok_or("share")?
                        );
                    }
                }
            }
        }
        for (&parent, ids) in &direct {
            let children = snapshot.children(parent).ok_or("children")?;
            assert_eq!(
                children.get(..ids.len()),
                Some(ids.as_slice()),
                "child order"
            );
        }
        for id in seen {
            let selected = direct.get(&id).map_or(0, Vec::len);
            let children = snapshot.children(id).ok_or("children")?;
            if selected != children.len() {
                assert!(
                    collapsed.contains(&id) || budget == 1,
                    "missing suffix for {id:?}"
                );
            }
        }
        Ok(())
    }
}

#[test]
fn summary_wide_thresholds_and_budget_account_for_every_row() -> TestResult {
    let snapshot = SummaryTree::build(&[
        (1000, 0, false),
        (500, 0, false),
        (100, 0, false),
        (50, 0, false),
    ])?;
    let children = snapshot.children(NodeId::ROOT).ok_or("children")?;
    let all = snapshot.summarize(NodeId::ROOT, usize::MAX, 0)?;
    assert_eq!(all.rows.len(), 5);
    assert!(!all.truncated);
    let threshold = snapshot.summarize(NodeId::ROOT, 60, 1000)?;
    assert_eq!(
        threshold.rows,
        vec![
            SummaryRow::Node {
                id: NodeId::ROOT,
                depth: 0
            },
            SummaryRow::Node {
                id: *children.first().ok_or("first")?,
                depth: 1
            },
            SummaryRow::Node {
                id: *children.get(1).ok_or("second")?,
                depth: 1
            },
            SummaryRow::Collapsed {
                parent: NodeId::ROOT,
                depth: 1,
                count: 2,
                alloc: 150,
                size: 156
            },
        ]
    );
    assert!(!threshold.truncated);
    for budget in 1..=8 {
        for bp in [0, 1000, 10_000] {
            let summary = snapshot.summarize(NodeId::ROOT, budget, bp)?;
            SummaryTree::verify(&snapshot, NodeId::ROOT, &summary, budget, bp)?;
        }
    }
    let hundred = snapshot.summarize(NodeId::ROOT, 400, 10_000)?;
    assert_eq!(hundred.rows.len(), 2);
    assert!(!hundred.truncated);
    let wide = SummaryTree::build(&vec![(1, 0, false); 10_000])?;
    let summary = wide.summarize(NodeId::ROOT, 60, 0)?;
    assert_eq!(summary.rows.len(), 60);
    assert!(summary.truncated);
    assert!(matches!(
        summary.rows.last(),
        Some(SummaryRow::Collapsed {
            count: 9942,
            alloc: 9942,
            size: 39768,
            ..
        })
    ));
    Ok(())
}

#[test]
fn summary_deep_chain_is_iterative_and_preserves_exact_threshold_equality() -> TestResult {
    let mut entries = vec![root()];
    let mut parent = 5;
    for record in 64..10_064 {
        let mut item = entry(record, parent, "d", true);
        item.alloc = 0;
        item.size = 0;
        entries.push(item);
        parent = record;
    }
    entries.push(entry(10_064, parent, "leaf", false));
    let snapshot = finish(entries, &ScanContext::default())?;
    let under = snapshot.resolve_path("/d")?;
    for bp in [0, 10_000] {
        for budget in [1, 2, 60, 400, 10_002] {
            let summary = snapshot.summarize(under, budget, bp)?;
            SummaryTree::verify(&snapshot, under, &summary, budget, bp)?;
            assert_eq!(summary.rows.len(), budget.min(10_001));
            assert_eq!(summary.truncated, budget < 10_001);
        }
    }
    Ok(())
}

#[test]
fn summary_zero_allocation_ignores_threshold_and_validates_arguments() -> TestResult {
    let snapshot = SummaryTree::build(&[(0, 0, true), (0, 1, false), (0, 0, false)])?;
    for budget in [1, 2, 3, 4, 60] {
        let zero = snapshot.summarize(NodeId::ROOT, budget, 0)?;
        assert_eq!(zero, snapshot.summarize(NodeId::ROOT, budget, 10_000)?);
        SummaryTree::verify(&snapshot, NodeId::ROOT, &zero, budget, 0)?;
    }
    assert!(matches!(
        snapshot.summarize(NodeId::ROOT, 0, 0),
        Err(QueryError::InvalidArgument { .. })
    ));
    let invalid = NodeId::from_raw(u32::MAX);
    assert!(
        matches!(snapshot.summarize(invalid, 1, 0), Err(QueryError::InvalidNode { node }) if node == invalid)
    );
    let empty = SummaryTree::build(&[])?;
    assert_eq!(empty.summarize(NodeId::ROOT, usize::MAX, 0)?.rows.len(), 1);
    let file = snapshot.resolve_path("/n-2")?;
    assert_eq!(
        snapshot.summarize(file, 10, 0)?.rows,
        vec![SummaryRow::Node { id: file, depth: 0 }]
    );
    Ok(())
}

#[test]
fn summary_returns_preorder_when_heap_selects_other_branches_first() -> TestResult {
    let snapshot = SummaryTree::build(&[
        (0, 0, true),
        (0, 0, true),
        (90, 1, false),
        (80, 2, false),
        (70, 1, false),
    ])?;
    let summary = snapshot.summarize(NodeId::ROOT, 60, 0)?;
    let names: Vec<_> = summary
        .rows
        .iter()
        .filter_map(|row| match row {
            SummaryRow::Node { id, depth } => Some((snapshot.name(*id), *depth)),
            _ => None,
        })
        .collect();
    assert_eq!(
        names,
        vec![
            (Some("/"), 0),
            (Some("n-0"), 1),
            (Some("n-2"), 2),
            (Some("n-4"), 2),
            (Some("n-1"), 1),
            (Some("n-3"), 2)
        ]
    );
    SummaryTree::verify(&snapshot, NodeId::ROOT, &summary, 60, 0)
}

proptest! {
    #[test]
    fn summaries_preserve_tree_prefixes_and_collapsed_accounting(
        spec in prop::collection::vec((0u16..1000, any::<u8>(), any::<bool>()), 0..100),
        budget in 1usize..120,
        bp in 0u32..=10_000,
    ) {
        let verify = || -> TestResult {
            let snapshot = SummaryTree::build(&spec)?;
            for root in [NodeId::ROOT, NodeId::from_raw(u32::try_from(snapshot.len().saturating_sub(1))?)] {
                let summary = snapshot.summarize(root, budget, bp)?;
                SummaryTree::verify(&snapshot, root, &summary, budget, bp)?;
                assert_eq!(summary, snapshot.summarize(root, budget, bp)?, "determinism");
            }
            Ok(())
        };
        verify().map_err(|error| TestCaseError::fail(error.to_string()))?;
    }
}

#[test]
fn summary_collapsed_totals_include_duplicate_links_and_check_overflow() -> TestResult {
    use crate::graph::entry::EntryName;
    use adamite_ntfs::record::reference::FileRef;
    for alloc in [512, u64::MAX] {
        let mut root = root();
        root.size = 0;
        root.alloc = 0;
        let mut file = entry(64, 5, "primary", false);
        file.size = 10;
        file.alloc = alloc;
        file.names.push(EntryName {
            parent: FileRef::from_raw((7u64 << 48) | 5),
            namespace: 1,
            name: "alias".encode_utf16().collect(),
        });
        let snapshot = finish(vec![root, file], &ScanContext::default())?;
        assert_eq!(snapshot.summarize(NodeId::ROOT, 1, 0)?.rows.len(), 1);
        if alloc == u64::MAX {
            assert!(matches!(
                snapshot.summarize(NodeId::ROOT, 2, 0),
                Err(QueryError::Snapshot {
                    source: crate::snapshot::SnapshotError::Overflow { .. }
                })
            ));
        } else {
            let summary = snapshot.summarize(NodeId::ROOT, 2, 0)?;
            assert!(matches!(
                summary.rows.last(),
                Some(SummaryRow::Collapsed {
                    count: 2,
                    alloc: 1024,
                    size: 20,
                    ..
                })
            ));
            SummaryTree::verify(&snapshot, NodeId::ROOT, &summary, 2, 0)?;
        }
    }
    Ok(())
}
