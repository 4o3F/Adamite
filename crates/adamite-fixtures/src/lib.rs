//! Shared NTFS image fixtures, independent reference MFT reconstruction and synthetic encoders.

pub mod builders;
mod fixture;
pub mod runlist_encoder;

pub use fixture::{
    Deleted, Entry, EntryKind, Extent, Fill, Fixture, FixtureError, Geometry, Manifest, Mft, Name,
    ReferenceMft, Stream,
};
