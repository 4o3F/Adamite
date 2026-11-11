# adamite-fixtures

`adamite-fixtures` is unpublished development support shared by workspace tests. `Fixture::load` reads a committed JSON manifest and its `.img.zst`, verifies decompressed length and checksum, and exposes both through `manifest()` and `image()`. `ReferenceMft` reconstructs reference MFT bytes independently of core; `builders` and `runlist_encoder` create synthetic test inputs.

Normal tests use committed fixtures without mounts or system NTFS tools. On Linux, `deno task fixtures` regenerates images and independent v3 manifests using ntfs-3g, zstd, attr and sudo for mounting. Review images and manifests together. See the [crate documentation](src/lib.rs) for API examples and the [architecture](../../docs/ARCHITECTURE.md) for fixture verification.
