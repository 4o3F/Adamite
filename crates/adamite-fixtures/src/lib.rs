//! Shared NTFS image fixtures, independent reference MFT reconstruction and synthetic encoders.

pub mod builders;
mod fixture;
mod manifest;
pub mod runlist_encoder;

pub use fixture::{Fixture, FixtureError, ReferenceMft};
pub use manifest::{
    Deleted, Entry, EntryKind, Extent, Fill, Geometry, Manifest, Mft, Name, Stream,
};
