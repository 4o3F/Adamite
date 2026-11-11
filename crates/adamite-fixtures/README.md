# adamite-fixtures

`adamite-fixtures` is unpublished development support shared by workspace tests. `Fixture::load` reads a committed JSON manifest and its `.img.zst`, verifies decompressed length and checksum, and exposes both through `manifest()` and `image()`. `ReferenceMft` reconstructs reference MFT bytes independently of core; `builders` and `runlist_encoder` create synthetic test inputs.

Normal tests use committed fixtures without mounts or system NTFS tools. On Linux, `deno task fixtures` regenerates images and independent v3 manifests using ntfs-3g, zstd, attr and sudo for mounting. Review images and manifests together. See the [crate documentation](src/lib.rs) for API examples and the [architecture](../../docs/ARCHITECTURE.md) for fixture verification.

## Streaming synthetic volumes

`VolumeImage` validates a boot geometry and writes its boot sector, sparse logical length and records to any empty `Write + Seek`. It also builds contiguous non-resident attributes using that geometry. Scanner test support and `SyntheticVolume` share this construction.

`SyntheticConfig::reference(in_use_records)` selects the scan-resource reference mix. Its fields configure the total slot count, exact in-use count (and thus occupancy), directory percentage, primary UTF-16 name length, DOS-alias percentage and non-resident DATA percentage. `SyntheticVolume::new(config)?.write(&mut writer)` streams records and 4 KiB bitmap blocks, leaving payload extents as sparse holes. Use a buffered file writer for large images. The writer must initially be empty. `image_name()` includes a generator version and all configuration fields for cache reuse; `len_bytes()` and `mft_bytes()` expose the calculated sizes. The benchmark binary documents the default workload and commands.
