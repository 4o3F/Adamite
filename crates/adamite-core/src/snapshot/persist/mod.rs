//! Canonical snapshot persistence without file I/O.

mod decode;
mod encode;
mod error;
mod format;
mod validate;

pub use decode::SnapshotDecoder;
pub use encode::SnapshotEncoder;
pub use error::PersistError;
