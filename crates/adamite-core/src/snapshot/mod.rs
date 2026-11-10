//! Immutable snapshot storage and bounded arena construction.

mod build;
mod error;
mod indexes;
mod node;
mod storage;

pub(crate) use build::{
    Aggregator, Finalizer, NameRange, ParentTree, ResolvedNode, SnapshotBuilder,
};
pub use error::SnapshotError;
pub use indexes::ExtensionStat;
pub use node::{Node, NodeFlags, NodeId, NodeView};
pub use storage::{Snapshot, VolumeInfo};

#[cfg(test)]
mod tests;
