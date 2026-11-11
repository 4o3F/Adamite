use crate::{error::CliError, render::time::ScanTime};
use adamite_core::{
    scan::{ScanOutput, ScanStats},
    snapshot::{NodeId, SnapshotError},
};
use serde::{Serialize, Serializer, ser::SerializeMap};
use std::{borrow::Cow, path::Path};

#[derive(Debug, Serialize)]
pub(crate) struct InfoReport<'a> {
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
pub(crate) struct Envelope<'a> {
    pub(crate) schema: &'static str,
    pub(crate) command: &'static str,
    pub(crate) snapshot: SnapshotReport<'a>,
    pub(crate) units: Units,
}

#[derive(Debug, Serialize)]
pub(crate) struct SnapshotReport<'a> {
    pub(crate) source: Cow<'a, str>,
    pub(crate) scanned_at: Option<String>,
    pub(crate) scanned_at_filetime: Option<String>,
    pub(crate) volume_serial: String,
    pub(crate) label: Option<&'a str>,
}

#[derive(Debug, Serialize)]
pub(crate) struct Units {
    size: &'static str,
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
        let root = snapshot.node(NodeId::ROOT).ok_or(CliError::Snapshot {
            source: SnapshotError::InvalidTree {
                reason: "missing root",
            },
        })?;
        Ok(Self {
            envelope: Envelope::new(command, source, output),
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
                scanned_at: ScanTime(context.scanned_at_filetime).rfc3339(),
                scanned_at_filetime: context.scanned_at_filetime.map(|value| value.to_string()),
                volume_serial: format!("{:016x}", volume.geometry().serial()),
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
        let stats = self.0;
        [
            ("records_total", stats.records_total),
            ("records_in_use", stats.records_in_use),
            ("base_records", stats.base_records),
            ("extension_records", stats.extension_records),
            ("unused", stats.unused),
            ("deleted", stats.deleted),
            ("baad", stats.baad),
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
            ("orphans", stats.orphans),
            ("cycle_nodes", stats.cycle_nodes),
            ("hardlink_duplicates", stats.hardlink_duplicates),
            ("metafiles", stats.metafiles),
        ]
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
