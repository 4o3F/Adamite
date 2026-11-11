use crate::{ByteSize, Escaped};
use adamite_core::{scan::ScanOutput, snapshot::NodeId};
use std::fmt;

/// Displays a volume serial as sixteen lowercase hexadecimal digits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VolumeSerial(
    /// Serial number from the parsed volume geometry.
    pub u64,
);

impl fmt::Display for VolumeSerial {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:016x}", self.0)
    }
}

/// Ordered volume, tree and nonzero scan-counter fields for text interfaces.
#[derive(Debug, Clone, Copy)]
pub struct InfoFields<'a> {
    /// Completed scan whose metadata and counters are displayed.
    pub output: &'a ScanOutput,
    /// Displays byte counts as raw integers when set.
    pub raw_bytes: bool,
}

impl InfoFields<'_> {
    /// Formats fields in CLI text order, omitting absent metadata and zero counters.
    #[must_use]
    pub fn rows(&self) -> Vec<(&'static str, String)> {
        let snapshot = &self.output.snapshot;
        let volume = snapshot.volume();
        let geometry = volume.geometry();
        let context = volume.context();
        let mut rows = vec![("Volume serial", VolumeSerial(geometry.serial()).to_string())];
        if let Some(label) = context.label.as_deref() {
            rows.push(("Label", Escaped(label).to_string()));
        }
        rows.extend([
            (
                "Bytes per sector",
                self.size(u64::from(geometry.bytes_per_sector())),
            ),
            (
                "Cluster size",
                self.size(u64::from(geometry.cluster_size())),
            ),
            ("Volume size", self.size(geometry.total_bytes())),
        ]);
        if let Some(free) = context.free_bytes {
            rows.push(("Free space", self.size(free)));
        }
        rows.push(("Nodes", snapshot.len().to_string()));
        if let Some(root) = snapshot.node(NodeId::ROOT) {
            rows.extend([
                ("Files", root.file_count().to_string()),
                ("Logical size", self.size(root.total_size())),
                ("Allocated size", self.size(root.total_alloc())),
            ]);
        }
        rows.extend(
            self.output
                .stats
                .counters()
                .into_iter()
                .filter(|(_, value)| *value != 0)
                .map(|(label, value)| (label, value.to_string())),
        );
        rows
    }

    fn size(&self, value: u64) -> String {
        ByteSize {
            value: u128::from(value),
            raw_bytes: self.raw_bytes,
        }
        .to_string()
    }
}
