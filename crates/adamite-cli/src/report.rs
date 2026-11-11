use crate::{error::CliError, locate::Locator};
use adamite_core::{
    scan::{ScanOutput, ScanStats},
    snapshot::{ExtensionStat, KindStat, NodeId, SearchPage, Snapshot},
};
use adamite_format::{ScanTime, VolumeSerial};
use serde::{Serialize, Serializer, ser::SerializeMap};
use std::{borrow::Cow, path::Path};

#[derive(Debug, Serialize)]
pub(crate) struct InfoReport<'a> {
    #[serde(skip)]
    pub(crate) output: &'a ScanOutput,
    #[serde(flatten)]
    pub(crate) envelope: Envelope<'a>,
    pub(crate) volume: Volume,
    pub(crate) tree: Tree,
    pub(crate) stats: Stats<'a>,
}

#[derive(Debug, Serialize)]
pub(crate) struct SavedReport<'a> {
    #[serde(flatten)]
    pub(crate) envelope: Envelope<'a>,
    pub(crate) saved: Cow<'a, str>,
    pub(crate) bytes: usize,
}

#[derive(Debug, Serialize)]
pub(crate) struct TopReport<'a> {
    #[serde(flatten)]
    pub(crate) envelope: Envelope<'a>,
    pub(crate) items: Vec<TopItem>,
}

#[derive(Debug, Serialize)]
pub(crate) struct TopItem {
    pub(crate) path: String,
    pub(crate) size: u64,
    pub(crate) alloc: u64,
    /// Decimal FILETIME, including zero; the core stores a raw timestamp for every file.
    pub(crate) mtime_filetime: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct ExtReport<'a> {
    #[serde(flatten)]
    pub(crate) envelope: Envelope<'a>,
    pub(crate) items: Vec<ExtItem<'a>>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ExtItem<'a> {
    pub(crate) extension: &'a str,
    pub(crate) count: u64,
    pub(crate) total_size: u64,
    pub(crate) total_alloc: u64,
}

#[derive(Debug, Serialize)]
pub(crate) struct KindReport<'a> {
    #[serde(flatten)]
    pub(crate) envelope: Envelope<'a>,
    pub(crate) items: Vec<KindItem>,
}

#[derive(Debug, Serialize)]
pub(crate) struct KindItem {
    pub(crate) kind: &'static str,
    pub(crate) count: u64,
    pub(crate) total_size: u64,
    pub(crate) total_alloc: u64,
}

#[derive(Debug, Serialize)]
pub(crate) struct SearchReport<'a> {
    #[serde(flatten)]
    pub(crate) envelope: Envelope<'a>,
    pub(crate) items: Vec<SearchItem<'a>>,
    pub(crate) total_matches: u64,
    pub(crate) next_offset: Option<usize>,
    #[serde(skip)]
    pub(crate) shown: Option<(usize, usize)>,
}

#[derive(Debug, Serialize)]
pub(crate) struct SearchItem<'a> {
    pub(crate) path: String,
    #[serde(flatten)]
    pub(crate) node: LsItem<'a>,
}

#[derive(Debug, Serialize)]
pub(crate) struct LsReport<'a> {
    #[serde(flatten)]
    pub(crate) envelope: Envelope<'a>,
    pub(crate) path: String,
    pub(crate) total_children: usize,
    pub(crate) offset: usize,
    pub(crate) limit: usize,
    pub(crate) items: Vec<LsItem<'a>>,
}

/// Child metadata with flags in core bit order: directory, reparse, sparse, compressed, hidden, system, hardlink_dup, synthetic. Unset flags are omitted.
#[derive(Debug, Serialize)]
pub(crate) struct LsItem<'a> {
    pub(crate) name: &'a str,
    pub(crate) kind: &'static str,
    pub(crate) flags: Vec<&'static str>,
    pub(crate) size: u64,
    pub(crate) alloc: u64,
    pub(crate) total_size: u64,
    pub(crate) total_alloc: u64,
    pub(crate) file_count: u32,
}

impl<'a> TopReport<'a> {
    pub(crate) fn new(source: &'a Path, output: &'a ScanOutput, n: u32) -> Result<Self, CliError> {
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

    pub(crate) fn from_ids(
        source: &'a Path,
        output: &'a ScanOutput,
        ids: impl IntoIterator<Item = NodeId>,
    ) -> Result<Self, CliError> {
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
    fn new(snapshot: &Snapshot, id: NodeId) -> Result<Self, CliError> {
        let node = snapshot.node(id).ok_or(CliError::InconsistentSnapshot {
            reason: "missing top file",
        })?;
        Ok(Self {
            path: node_path(snapshot, id)?,
            size: node.size(),
            alloc: node.alloc(),
            mtime_filetime: node.mtime().to_string(),
        })
    }
}

impl<'a> ExtReport<'a> {
    pub(crate) fn new(source: &'a Path, output: &'a ScanOutput, n: u64) -> Self {
        Self::from_stats(source, output, output.snapshot.ext_stats(), n)
    }

    pub(crate) fn from_stats(
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
    pub(crate) fn new(
        source: &'a Path,
        output: &'a ScanOutput,
        stats: &[KindStat],
        n: u64,
    ) -> Self {
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
    pub(crate) fn new(
        source: &'a Path,
        output: &'a ScanOutput,
        page: &SearchPage,
        offset: usize,
    ) -> Result<Self, CliError> {
        let shown = if page.items().is_empty() {
            None
        } else {
            let start = offset.checked_add(1).ok_or(CliError::Usage {
                message: "search page exceeds the supported range",
            })?;
            let end = offset
                .checked_add(page.items().len())
                .ok_or(CliError::Usage {
                    message: "search page exceeds the supported range",
                })?;
            Some((start, end))
        };
        let items = page
            .items()
            .iter()
            .map(|&id| {
                Ok(SearchItem {
                    path: node_path(&output.snapshot, id)?,
                    node: LsItem::new(&output.snapshot, id)?,
                })
            })
            .collect::<Result<_, CliError>>()?;
        Ok(Self {
            envelope: Envelope::new("search", source, output),
            items,
            total_matches: page.total_matches(),
            next_offset: page.next_offset(),
            shown,
        })
    }
}

fn node_path(snapshot: &Snapshot, id: NodeId) -> Result<String, CliError> {
    snapshot
        .path(id)
        .map_err(|source| CliError::Snapshot { source })?
        .ok_or(CliError::InconsistentSnapshot {
            reason: "missing node path",
        })
}

impl<'a> LsReport<'a> {
    pub(crate) fn new(
        source: &'a Path,
        output: &'a ScanOutput,
        path: &str,
        offset: usize,
        limit: usize,
    ) -> Result<Self, CliError> {
        let end = offset.checked_add(limit).ok_or(CliError::Usage {
            message: "--offset + --limit exceeds the supported range",
        })?;
        let snapshot = &output.snapshot;
        let locator = Locator::new(snapshot);
        let id = locator.resolve(path)?;
        let children = locator.directory(id)?;
        let path = node_path(snapshot, id)?;
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
    fn new(snapshot: &'a Snapshot, id: NodeId) -> Result<Self, CliError> {
        let node = snapshot.node(id).ok_or(CliError::InconsistentSnapshot {
            reason: "missing child",
        })?;
        Ok(Self {
            name: snapshot.name(id).ok_or(CliError::InconsistentSnapshot {
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

#[derive(Debug, Serialize)]
pub(crate) struct Envelope<'a> {
    pub(crate) schema: &'static str,
    pub(crate) command: &'static str,
    pub(crate) snapshot: SnapshotReport<'a>,
    pub(crate) units: Units,
}

#[derive(Debug, Serialize)]
pub(crate) struct SnapshotReport<'a> {
    pub(crate) source: Cow<'a, str>,
    #[serde(serialize_with = "SnapshotReport::serialize_time")]
    pub(crate) scanned_at: ScanTime,
    pub(crate) scanned_at_filetime: Option<String>,
    pub(crate) volume_serial: String,
    pub(crate) label: Option<&'a str>,
}

#[derive(Debug, Serialize)]
pub(crate) struct Units {
    size: &'static str,
}

impl SnapshotReport<'_> {
    fn serialize_time<S: Serializer>(time: &ScanTime, serializer: S) -> Result<S::Ok, S::Error> {
        time.rfc3339().serialize(serializer)
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct Volume {
    pub(crate) bytes_per_sector: u16,
    pub(crate) cluster_size: u32,
    pub(crate) total_bytes: u64,
    pub(crate) free_bytes: Option<u64>,
}

#[derive(Debug, Serialize)]
pub(crate) struct Tree {
    pub(crate) nodes: usize,
    pub(crate) files: u32,
    pub(crate) total_size: u64,
    pub(crate) total_alloc: u64,
}

#[derive(Debug)]
pub(crate) struct Stats<'a>(&'a ScanStats);

impl<'a> InfoReport<'a> {
    pub(crate) fn new(
        command: &'static str,
        source: &'a Path,
        output: &'a ScanOutput,
    ) -> Result<Self, CliError> {
        let snapshot = &output.snapshot;
        let geometry = snapshot.volume().geometry();
        let context = snapshot.volume().context();
        let root = snapshot
            .node(NodeId::ROOT)
            .ok_or(CliError::InconsistentSnapshot {
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
    pub(crate) fn new(command: &'static str, source: &'a Path, output: &'a ScanOutput) -> Self {
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
    pub(crate) fn new(
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
    pub(crate) fn entries(&self) -> [(&'static str, u64); 26] {
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
