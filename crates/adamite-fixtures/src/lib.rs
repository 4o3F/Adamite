//! Shared NTFS image fixtures, independent reference MFT reconstruction and synthetic encoders.
//!
//! This unpublished development crate is independent of core. [`Fixture::load`] reads a JSON manifest and its sibling `.img.zst`, verifies decompressed length and frame checksum, and exposes [`Fixture::image`] and [`Fixture::manifest`]. [`Fixture::from_image`] accepts already decompressed bytes. [`ReferenceMft`] provides separate parser-test reconstruction; core tests feed image ranges directly to the public scanner.
//!
//! Committed manifests use schema v3: stream type, independently measured allocation/compressed size, metafile entries and filler totals accompany paths, names and metadata. The reader also accepts v2, leaving absent measurements unset. [`builders`] and [`runlist_encoder`] construct independent synthetic input bytes.
//!
//! [`MappedAllocation::from_reference`] independently counts mapped bytes across each non-resident stream's segments in a [`ReferenceMft`]. [`MappedAllocation::stream_bytes`] looks up totals by base record, attribute type and raw UTF-16 name, counting physical mappings while excluding holes.
//!
//! # Examples
//!
//! ```
//! use adamite_fixtures::Fixture;
//! use std::path::Path;
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let manifest = Path::new(env!("CARGO_MANIFEST_DIR"))
//!     .join("../../tests/fixtures/ntfs/basic-4k.json");
//! let fixture = Fixture::load(&manifest)?;
//! assert_eq!(fixture.manifest().schema_version, 3);
//! assert_eq!(u64::try_from(fixture.image().len())?, fixture.manifest().image_len);
//! # Ok(())
//! # }
//! ```
//!
//! # Regenerating fixtures
//!
//! From the repository root on Linux, run `deno task fixtures` with ntfs-3g, zstd and attr installed. Generation uses sudo for mounting and writes independent v3 manifests plus compressed images under `tests/fixtures/ntfs/`. Review both files together after regeneration. Normal Rust/Deno tests use committed data without mounts or system NTFS tools.

pub mod builders;
mod fixture;
mod manifest;
mod mapped_allocation;
pub mod runlist_encoder;
mod synthetic_volume;
mod volume_image;

pub use fixture::{Fixture, FixtureError, ReferenceMft};
pub use manifest::{
    Deleted, Entry, EntryKind, Extent, Fill, Geometry, Manifest, Mft, Name, Stream,
};
pub use mapped_allocation::MappedAllocation;
pub use synthetic_volume::{SyntheticConfig, SyntheticVolume};
pub use volume_image::VolumeImage;
