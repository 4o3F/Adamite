//! Serializable reports constructed from scan output and snapshot queries.

use crate::{DirectoryLookup, ReportError};
use adamite_core::{
    scan::{ScanOutput, ScanStats},
    snapshot::{ExtensionStat, FileFilter, KindStat, NodeId, SearchPage, Snapshot},
};
use adamite_format::{ScanTime, VolumeSerial};
use serde::{Serialize, Serializer, ser::SerializeMap};
use std::{borrow::Cow, path::Path};

/// Volume metadata, tree totals and scan counters.
#[derive(Debug, Serialize)]
pub struct InfoReport<'a> {
    /// Scan output used for text rendering.
    #[serde(skip)]
    pub output: &'a ScanOutput,
    /// Shared report metadata.
    #[serde(flatten)]
    pub envelope: Envelope<'a>,
    /// Volume geometry and capacity.
    pub volume: Volume,
    /// Snapshot tree totals.
    pub tree: Tree,
    /// Scan diagnostics.
    pub stats: Stats<'a>,
}

/// Confirmation of a published snapshot.
#[derive(Debug, Serialize)]
pub struct SavedReport<'a> {
    /// Shared report metadata.
    #[serde(flatten)]
    pub envelope: Envelope<'a>,
    /// Published snapshot path.
    pub saved: Cow<'a, str>,
    /// Snapshot file length in bytes.
    pub bytes: usize,
}

/// Ranked file paths and their sizes.
#[derive(Debug, Serialize)]
pub struct TopReport<'a> {
    /// Shared report metadata.
    #[serde(flatten)]
    pub envelope: Envelope<'a>,
    /// Rows in report order.
    pub items: Vec<TopItem>,
}

/// Size and modification time of a ranked file.
#[derive(Debug, Serialize)]
pub struct TopItem {
    /// Canonical snapshot path.
    pub path: String,
    /// Logical size in bytes.
    pub size: u64,
    /// Allocated size in bytes.
    pub alloc: u64,
    /// Decimal FILETIME, including zero; the core stores a raw timestamp for every file.
    pub mtime_filetime: String,
}

/// File totals grouped by extension.
#[derive(Debug, Serialize)]
pub struct ExtReport<'a> {
    /// Shared report metadata.
    #[serde(flatten)]
    pub envelope: Envelope<'a>,
    /// Rows in report order.
    pub items: Vec<ExtItem<'a>>,
}

/// Counts and sizes for one extension.
#[derive(Debug, Serialize)]
pub struct ExtItem<'a> {
    /// Extension without a leading dot; empty for files without an extension.
    pub extension: &'a str,
    /// Number of primary files.
    pub count: u64,
    /// Logical size including descendants, in bytes.
    pub total_size: u64,
    /// Allocated size including descendants, in bytes.
    pub total_alloc: u64,
}

/// File totals grouped by category.
#[derive(Debug, Serialize)]
pub struct KindReport<'a> {
    /// Shared report metadata.
    #[serde(flatten)]
    pub envelope: Envelope<'a>,
    /// Rows in report order.
    pub items: Vec<KindItem>,
}

/// Counts and sizes for one file category.
#[derive(Debug, Serialize)]
pub struct KindItem {
    /// File category or node kind.
    pub kind: &'static str,
    /// Number of primary files.
    pub count: u64,
    /// Logical size including descendants, in bytes.
    pub total_size: u64,
    /// Allocated size including descendants, in bytes.
    pub total_alloc: u64,
}

/// One page of matching snapshot nodes.
#[derive(Debug, Serialize)]
pub struct SearchReport<'a> {
    /// Shared report metadata.
    #[serde(flatten)]
    pub envelope: Envelope<'a>,
    /// Rows in report order.
    pub items: Vec<SearchItem<'a>>,
    /// Total matches before pagination.
    pub total_matches: u64,
    /// Offset of the next page, when more matches exist.
    pub next_offset: Option<usize>,
    /// Inclusive one-based range displayed in text output.
    #[serde(skip)]
    pub shown: Option<(usize, usize)>,
}

/// Canonical path and metadata of a matching node.
#[derive(Debug, Serialize)]
pub struct SearchItem<'a> {
    /// Canonical snapshot path.
    pub path: String,
    /// Matching node metadata.
    #[serde(flatten)]
    pub node: LsItem<'a>,
}

/// One page of a directory’s children.
#[derive(Debug, Serialize)]
pub struct LsReport<'a> {
    /// Shared report metadata.
    #[serde(flatten)]
    pub envelope: Envelope<'a>,
    /// Canonical snapshot path.
    pub path: String,
    /// Number of children before pagination.
    pub total_children: usize,
    /// Number of children skipped.
    pub offset: usize,
    /// Requested maximum number of children.
    pub limit: usize,
    /// Rows in report order.
    pub items: Vec<LsItem<'a>>,
}

/// Child metadata with flags in core bit order: directory, reparse, sparse, compressed, hidden, system, hardlink_dup, synthetic. Unset flags are omitted.
#[derive(Debug, Serialize)]
pub struct LsItem<'a> {
    /// Snapshot node name.
    pub name: &'a str,
    /// File category or node kind.
    pub kind: &'static str,
    /// Set node flags in core bit order.
    pub flags: Vec<&'static str>,
    /// Logical size in bytes.
    pub size: u64,
    /// Allocated size in bytes.
    pub alloc: u64,
    /// Logical size including descendants, in bytes.
    pub total_size: u64,
    /// Allocated size including descendants, in bytes.
    pub total_alloc: u64,
    /// Number of primary files in the subtree.
    pub file_count: u32,
}

impl<'a> TopReport<'a> {
    /// Reports up to `n` files from the snapshot’s stored top-file index.
    ///
    /// # Errors
    /// Returns a snapshot tree error or an inconsistency error for a missing file or path.
    pub fn new(source: &'a Path, output: &'a ScanOutput, n: u32) -> Result<Self, ReportError> {
        Self::from_ids(
            source,
            output,
            output
                .snapshot
                .top_files()
                .iter()
                .copied()
                .zip(0..n)
                .map(|(id, _)| id),
        )
    }

    /// Uses the stored index without filters, otherwise ranks files beneath a directory.
    ///
    /// # Errors
    /// Returns a directory lookup, query or report construction error.
    pub fn select(
        source: &'a Path,
        output: &'a ScanOutput,
        n: u32,
        under: Option<&str>,
        filter: Option<&FileFilter>,
    ) -> Result<Self, ReportError> {
        if under.is_none() && filter.is_none() {
            return Self::new(source, output, n);
        }
        let root = output.snapshot.resolve_directory(under.unwrap_or("/"))?;
        let n = usize::try_from(n).map_err(|_| ReportError::Usage {
            message: "n exceeds the supported range",
        })?;
        let ids = output
            .snapshot
            .rank_files(root, filter.unwrap_or(&FileFilter::All), n)?;
        Self::from_ids(source, output, ids)
    }

    /// Builds ranked rows from node identifiers in the supplied order.
    ///
    /// # Errors
    /// Returns a snapshot tree error or an inconsistency error for a missing node or path.
    pub fn from_ids(
        source: &'a Path,
        output: &'a ScanOutput,
        ids: impl IntoIterator<Item = NodeId>,
    ) -> Result<Self, ReportError> {
        let items = ids
            .into_iter()
            .map(|id| TopItem::new(&output.snapshot, id))
            .collect::<Result<_, _>>()?;
        Ok(Self {
            envelope: Envelope::new("top", source, output),
            items,
        })
    }
}

impl TopItem {
    fn new(snapshot: &Snapshot, id: NodeId) -> Result<Self, ReportError> {
        let node = snapshot.node(id).ok_or(ReportError::InconsistentSnapshot {
            reason: "missing top file",
        })?;
        Ok(Self {
            path: LsItem::path(snapshot, id)?,
            size: node.size(),
            alloc: node.alloc(),
            mtime_filetime: node.mtime().to_string(),
        })
    }
}

impl<'a> ExtReport<'a> {
    /// Reports up to `n` rows from the snapshot’s stored extension statistics.
    pub fn new(source: &'a Path, output: &'a ScanOutput, n: u64) -> Self {
        Self::from_stats(source, output, output.snapshot.ext_stats(), n)
    }

    /// Builds extension rows from the supplied statistics in their existing order.
    pub fn from_stats(
        source: &'a Path,
        output: &'a ScanOutput,
        stats: &'a [ExtensionStat],
        n: u64,
    ) -> Self {
        Self {
            envelope: Envelope::new("ext", source, output),
            items: stats
                .iter()
                .zip(0..n)
                .map(|(stat, _)| ExtItem {
                    extension: stat.extension(),
                    count: stat.count(),
                    total_size: stat.total_size(),
                    total_alloc: stat.total_alloc(),
                })
                .collect(),
        }
    }
}

impl<'a> KindReport<'a> {
    /// Reports up to `n` category rows from the supplied statistics in their existing order.
    pub fn new(source: &'a Path, output: &'a ScanOutput, stats: &[KindStat], n: u64) -> Self {
        Self {
            envelope: Envelope::new("ext", source, output),
            items: stats
                .iter()
                .zip(0..n)
                .map(|(stat, _)| KindItem {
                    kind: stat.kind().name(),
                    count: stat.count(),
                    total_size: stat.total_size(),
                    total_alloc: stat.total_alloc(),
                })
                .collect(),
        }
    }
}

impl<'a> SearchReport<'a> {
    /// Builds search rows and the displayed range from a query page and its offset.
    ///
    /// # Errors
    /// Returns an error for an overflowing displayed range or missing node metadata or paths.
    pub fn new(
        source: &'a Path,
        output: &'a ScanOutput,
        page: &SearchPage,
        offset: usize,
    ) -> Result<Self, ReportError> {
        let shown = if page.items().is_empty() {
            None
        } else {
            let start = offset.checked_add(1).ok_or(ReportError::Usage {
                message: "search page exceeds the supported range",
            })?;
            let end = offset
                .checked_add(page.items().len())
                .ok_or(ReportError::Usage {
                    message: "search page exceeds the supported range",
                })?;
            Some((start, end))
        };
        let items = page
            .items()
            .iter()
            .map(|&id| {
                Ok(SearchItem {
                    path: LsItem::path(&output.snapshot, id)?,
                    node: LsItem::new(&output.snapshot, id)?,
                })
            })
            .collect::<Result<_, ReportError>>()?;
        Ok(Self {
            envelope: Envelope::new("search", source, output),
            items,
            total_matches: page.total_matches(),
            next_offset: page.next_offset(),
            shown,
        })
    }
}

impl<'a> LsReport<'a> {
    /// Resolves a directory and reports its children after applying offset and limit.
    ///
    /// # Errors
    /// Returns an error for an overflowing page range, an invalid directory path, or missing child metadata.
    pub fn new(
        source: &'a Path,
        output: &'a ScanOutput,
        path: &str,
        offset: usize,
        limit: usize,
    ) -> Result<Self, ReportError> {
        let end = offset.checked_add(limit).ok_or(ReportError::Usage {
            message: "--offset + --limit exceeds the supported range",
        })?;
        let snapshot = &output.snapshot;
        let id = snapshot.resolve_directory(path)?;
        let children = snapshot
            .children(id)
            .ok_or(ReportError::InconsistentSnapshot {
                reason: "missing directory children",
            })?;
        let path = LsItem::path(snapshot, id)?;
        let items = children
            .iter()
            .take(end)
            .skip(offset)
            .map(|&id| LsItem::new(snapshot, id))
            .collect::<Result<_, _>>()?;
        Ok(Self {
            envelope: Envelope::new("ls", source, output),
            path,
            total_children: children.len(),
            offset,
            limit,
            items,
        })
    }
}

impl<'a> LsItem<'a> {
    fn path(snapshot: &Snapshot, id: NodeId) -> Result<String, ReportError> {
        snapshot
            .path(id)
            .map_err(|source| ReportError::Snapshot { source })?
            .ok_or(ReportError::InconsistentSnapshot {
                reason: "missing node path",
            })
    }

    fn new(snapshot: &'a Snapshot, id: NodeId) -> Result<Self, ReportError> {
        let node = snapshot.node(id).ok_or(ReportError::InconsistentSnapshot {
            reason: "missing child",
        })?;
        Ok(Self {
            name: snapshot.name(id).ok_or(ReportError::InconsistentSnapshot {
                reason: "missing child name",
            })?,
            kind: if node.is_dir() { "dir" } else { "file" },
            flags: node.flags().names().collect(),
            size: node.size(),
            alloc: node.alloc(),
            total_size: node.total_size(),
            total_alloc: node.total_alloc(),
            file_count: node.file_count(),
        })
    }
}

/// Shared schema, command, source and unit metadata.
#[derive(Debug, Serialize)]
pub struct Envelope<'a> {
    /// JSON schema identifier.
    pub schema: &'static str,
    /// Command that produced the report.
    pub command: &'static str,
    /// Source identity and scan metadata.
    pub snapshot: SnapshotReport<'a>,
    /// Units for numeric sizes.
    pub units: Units,
}

/// Source identity and scan timestamp.
#[derive(Debug, Serialize)]
pub struct SnapshotReport<'a> {
    /// Original source path for this report.
    pub source: Cow<'a, str>,
    /// UTC scan timestamp used for display.
    #[serde(serialize_with = "SnapshotReport::serialize_time")]
    pub scanned_at: ScanTime,
    /// Raw decimal FILETIME when recorded.
    pub scanned_at_filetime: Option<String>,
    /// Sixteen-digit hexadecimal volume serial.
    pub volume_serial: String,
    /// Volume label when recorded.
    pub label: Option<&'a str>,
}

/// Units used by numeric report fields.
#[derive(Debug, Serialize)]
pub struct Units {
    size: &'static str,
}

impl SnapshotReport<'_> {
    fn serialize_time<S: Serializer>(time: &ScanTime, serializer: S) -> Result<S::Ok, S::Error> {
        time.rfc3339().serialize(serializer)
    }
}

/// Sector, cluster and capacity metadata.
#[derive(Debug, Serialize)]
pub struct Volume {
    /// Bytes in a volume sector.
    pub bytes_per_sector: u16,
    /// Bytes in an allocation cluster.
    pub cluster_size: u32,
    /// Total volume capacity in bytes.
    pub total_bytes: u64,
    /// Free capacity in bytes when recorded.
    pub free_bytes: Option<u64>,
}

/// Node counts and sizes for the snapshot tree.
#[derive(Debug, Serialize)]
pub struct Tree {
    /// Number of snapshot nodes.
    pub nodes: usize,
    /// Number of primary files.
    pub files: u32,
    /// Logical size including descendants, in bytes.
    pub total_size: u64,
    /// Allocated size including descendants, in bytes.
    pub total_alloc: u64,
}

/// Borrowed scan counters serialized in their core field order.
#[derive(Debug)]
pub struct Stats<'a>(&'a ScanStats);

impl<'a> InfoReport<'a> {
    /// Builds volume metadata, root totals and scan counters for `command`.
    ///
    /// # Errors
    /// Returns an inconsistency error when the snapshot root is missing.
    pub fn new(
        command: &'static str,
        source: &'a Path,
        output: &'a ScanOutput,
    ) -> Result<Self, ReportError> {
        let snapshot = &output.snapshot;
        let geometry = snapshot.volume().geometry();
        let context = snapshot.volume().context();
        let root = snapshot
            .node(NodeId::ROOT)
            .ok_or(ReportError::InconsistentSnapshot {
                reason: "missing root",
            })?;
        Ok(Self {
            envelope: Envelope::new(command, source, output),
            output,
            volume: Volume {
                bytes_per_sector: geometry.bytes_per_sector(),
                cluster_size: geometry.cluster_size(),
                total_bytes: geometry.total_bytes(),
                free_bytes: context.free_bytes,
            },
            tree: Tree {
                nodes: snapshot.len(),
                files: root.file_count(),
                total_size: root.total_size(),
                total_alloc: root.total_alloc(),
            },
            stats: Stats(&output.stats),
        })
    }
}

impl<'a> Envelope<'a> {
    /// Builds schema, command, source, timestamp and byte-unit metadata.
    pub fn new(command: &'static str, source: &'a Path, output: &'a ScanOutput) -> Self {
        let volume = output.snapshot.volume();
        let context = volume.context();
        Self {
            schema: "adamite/1",
            command,
            snapshot: SnapshotReport {
                source: source.to_string_lossy(),
                scanned_at: ScanTime(context.scanned_at_filetime),
                scanned_at_filetime: context.scanned_at_filetime.map(|value| value.to_string()),
                volume_serial: VolumeSerial(volume.geometry().serial()).to_string(),
                label: context.label.as_deref(),
            },
            units: Units { size: "bytes" },
        }
    }
}

impl<'a> SavedReport<'a> {
    /// Records the published snapshot destination and its encoded length.
    pub fn new(
        source: &'a Path,
        output: &'a ScanOutput,
        destination: &'a Path,
        bytes: usize,
    ) -> Self {
        Self {
            envelope: Envelope::new("scan", source, output),
            saved: destination.to_string_lossy(),
            bytes,
        }
    }
}

impl Stats<'_> {
    /// Returns scan counter names and values in core field order.
    pub fn entries(&self) -> [(&'static str, u64); 26] {
        self.0.counters()
    }
}

impl Serialize for Stats<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let entries = self.entries();
        let mut map = serializer.serialize_map(Some(entries.len()))?;
        for (key, value) in entries {
            map.serialize_entry(key, &value)?;
        }
        map.end()
    }
}

/// A budgeted, pre-order tree of subtree sizes and allocation shares.
#[derive(Debug, Serialize)]
pub struct TreeReport<'a> {
    /// Shared report metadata.
    #[serde(flatten)]
    pub envelope: Envelope<'a>,
    /// Individually selected nodes and collapsed sibling groups.
    pub rows: Vec<TreeItem<'a>>,
    /// Whether the budget prevented any node expansion.
    pub truncated: bool,
}

/// One tree row, with byte totals and percentage of the requested root allocation.
#[derive(Debug, Serialize)]
pub struct TreeItem<'a> {
    /// Canonical node path, or the parent path for a collapsed group.
    pub path: String,
    /// Node name, or `…` for a collapsed group.
    pub name: &'a str,
    /// Distance from the requested root; its depth is zero.
    pub depth: usize,
    /// Subtree allocation in bytes.
    pub alloc: u64,
    /// Subtree logical size in bytes.
    pub size: u64,
    /// Percentage of the requested root allocation; zero when its allocation is zero.
    pub share: f64,
    /// `node` or `collapsed`.
    pub kind: &'static str,
    /// Number of direct children represented by a collapsed row; absent on nodes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<usize>,
}

impl<'a> TreeReport<'a> {
    /// Resolves a directory and summarizes it within a row budget and share threshold.
    ///
    /// # Errors
    /// Returns a directory lookup, summary query, allocation or snapshot metadata error.
    pub fn new(
        source: &'a Path,
        output: &'a ScanOutput,
        path: &str,
        budget: usize,
        min_share_bp: u32,
    ) -> Result<Self, ReportError> {
        use adamite_core::snapshot::{QueryError, SummaryRow};

        let snapshot = &output.snapshot;
        let root = snapshot.resolve_directory(path)?;
        let denominator = snapshot
            .node(root)
            .ok_or(ReportError::InconsistentSnapshot {
                reason: "missing tree root",
            })?
            .total_alloc();
        let summary = snapshot.summarize(root, budget, min_share_bp)?;
        let mut rows = Vec::new();
        rows.try_reserve_exact(summary.rows.len())
            .map_err(QueryError::from)?;
        for row in summary.rows {
            let (id, depth, alloc, size, name, count) = match row {
                SummaryRow::Node { id, depth } => {
                    let node = snapshot.node(id).ok_or(ReportError::InconsistentSnapshot {
                        reason: "missing tree node",
                    })?;
                    let name = snapshot.name(id).ok_or(ReportError::InconsistentSnapshot {
                        reason: "missing tree name",
                    })?;
                    (id, depth, node.total_alloc(), node.total_size(), name, None)
                }
                SummaryRow::Collapsed {
                    parent,
                    depth,
                    count,
                    alloc,
                    size,
                } => (parent, depth, alloc, size, "…", Some(count)),
            };
            rows.push(TreeItem {
                path: LsItem::path(snapshot, id)?,
                name,
                depth,
                alloc,
                size,
                share: if denominator == 0 {
                    0.0
                } else {
                    alloc as f64 / denominator as f64 * 100.0
                },
                kind: if count.is_some() { "collapsed" } else { "node" },
                count,
            });
        }
        Ok(Self {
            envelope: Envelope::new("tree", source, output),
            rows,
            truncated: summary.truncated,
        })
    }
}

/// Metadata for one resolved file or directory, including named flags and hard links.
#[derive(Debug, Serialize)]
pub struct NodeReport<'a> {
    /// Shared report metadata.
    #[serde(flatten)]
    pub envelope: Envelope<'a>,
    /// Canonical volume-relative path.
    pub path: String,
    /// Snapshot-local node identifier.
    pub node_id: u32,
    /// On-disk hard-link count.
    pub hard_link_count: u16,
    /// Raw modification FILETIME when nonzero.
    pub mtime_filetime: Option<String>,
    /// UTC modification timestamp when representable.
    pub modified_at: Option<String>,
    /// Node name, kind, sizes, subtree totals and named flags.
    #[serde(flatten)]
    pub node: LsItem<'a>,
}

impl<'a> NodeReport<'a> {
    /// Resolves a node and reads the metadata retained in the snapshot.
    ///
    /// # Errors
    /// Returns a path lookup or missing metadata error.
    pub fn new(source: &'a Path, output: &'a ScanOutput, path: &str) -> Result<Self, ReportError> {
        let snapshot = &output.snapshot;
        let id = snapshot.resolve_path(path)?;
        let view = snapshot.node(id).ok_or(ReportError::InconsistentSnapshot {
            reason: "missing node",
        })?;
        let time = (view.mtime() != 0).then_some(view.mtime());
        Ok(Self {
            envelope: Envelope::new("node_info", source, output),
            path: LsItem::path(snapshot, id)?,
            node_id: id.raw(),
            hard_link_count: view.nlink(),
            mtime_filetime: time.map(|time| time.to_string()),
            modified_at: ScanTime(time).rfc3339(),
            node: LsItem::new(snapshot, id)?,
        })
    }
}
