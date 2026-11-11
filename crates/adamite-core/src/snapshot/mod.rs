//! Immutable snapshot storage and bounded arena construction.

mod build;
mod error;
mod indexes;
mod kind;
mod name_pattern;
mod node;
mod persist;
mod query;
mod rules;
mod storage;

pub(crate) use build::{
    Aggregator, Finalizer, NameRange, ParentTree, ResolvedNode, SnapshotBuilder,
};
pub use error::SnapshotError;
pub use indexes::{ExtensionStat, KindStat};
pub use kind::FileKind;
pub use name_pattern::NamePattern;
pub use node::{Node, NodeFlags, NodeId, NodeView};
pub use persist::{PersistError, SnapshotDecoder, SnapshotEncoder};
pub use query::{FileFilter, QueryError, SearchPage};
pub use storage::{Snapshot, VolumeInfo};

#[cfg(test)]
mod tests;
