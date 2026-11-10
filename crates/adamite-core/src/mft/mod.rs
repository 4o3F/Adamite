//! NTFS-specific reading, ingestion and merge stages.

pub(crate) mod bootstrap;
pub(crate) mod discovery;
pub(crate) mod ingest;
pub(crate) mod mapping;
pub(crate) mod merge;
pub(crate) mod model;

pub(crate) use bootstrap::Record0;
pub(crate) use discovery::{Discovery, DiscoveryStep};
pub(crate) use ingest::Ingest;
pub(crate) use mapping::{MftMapping, RecordAssembly};
pub(crate) use merge::Merge;
