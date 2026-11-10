//! Immutable snapshot storage and bounded arena construction.

mod builder;
mod indexes;
mod node;
mod storage;

pub use indexes::ExtensionStat;
pub(crate) use storage::SnapshotStorage;

pub(crate) use builder::SnapshotBuilder;
pub use node::{Node, NodeFlags, NodeId, NodeView};
pub use storage::{Snapshot, SnapshotError, VolumeInfo};
pub(crate) use storage::{checked_u32, name_range};

#[cfg(test)]
mod tests;
