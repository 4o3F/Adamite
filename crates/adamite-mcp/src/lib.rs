//! Snapshot discovery, selection and blocking load caching for MCP callers.
//!
//! [`SnapshotStore`] refreshes discovery through [`adamite_access::SnapshotCatalog`].
//! [`SnapshotCache`] shares decoded outputs and coordinates blocking loads without a runtime.

mod error;
mod snapshot_cache;
mod snapshot_store;

pub use error::McpError;
pub use snapshot_cache::SnapshotCache;
pub use snapshot_store::{SnapshotInfo, SnapshotOptions, SnapshotStore};

#[cfg(test)]
mod tests;
