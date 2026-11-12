# adamite-core

`adamite-core` scans caller-supplied NTFS bytes into an immutable `snapshot::Snapshot` and `scan::ScanStats`. It uses `std`, forbids unsafe code and depends on `adamite-ntfs`, crc32fast, globset, snafu and tracing. It performs no I/O and starts no threads or timers. Persistence accepts a caller-supplied sequential byte-copy callback; file access and seeking stay in `adamite-io`.

The public API lives under `scan::`, `snapshot::` and `error::`; the crate root exports modules only. Every scan item has one path directly under `scan::`, and its implementation children are private.

`ScanStats::counters()` returns all 26 counter names and values in field order. Names match the snake_case fields; snapshot encoding and display reports use this order. `NodeView::is_dir()` identifies directory entries, including synthetic directories. `NodeFlags::names()` yields the snake_case names of set flags in ascending bit order.

```text
src/
  lib.rs            crate docs, attributes and module declarations
  scan/             configuration, request protocol, status, stats and driver
  mft/              private bootstrap, discovery, mapping, ingest/decode and merge
  graph/            private entry model and parent/name/hard-link resolution
  snapshot/         immutable nodes, pooled names, indexes, volume and errors
    build/          private input, builder, aggregation and finalization passes
    persist/        private canonical .adm v1 encoder, decoder and validation
    rules.rs        private ranking and extension rules shared by build/persist/query
  error.rs          fatal ScanError and explicit domain constructors
  util.rs           private fallible collection and checked-counter traits
```

The driver owns request ids, queue capacity and waiting decisions. MFT stages expose byte-range planning and bounded consumption; graph produces snapshot-owned build inputs. Snapshot owns its storage and all passes that mutate it. Configuration and statistics remain independent leaves, and generic linking does not depend on NTFS contribution models.

## Driving a scan

```rust
use adamite_core::{
    error::ScanError,
    scan::{ScanConfig, ScanOutput, ScanStep, Scanner},
    snapshot::Snapshot,
};
```

1. Construct `Scanner::new(ScanConfig::default())`: one outstanding request and 1 MiB chunks. Chunks must be multiples of 512 between 512 bytes and 16 MiB.
2. Call `step()`. For `Need(ReadRequest { id, offset, len })`, acquire the exact volume-relative byte range and call `feed(id, bytes)`. Responses may arrive in any order; retained responses occupy their slots until consumed in virtual order.
3. Continue on `Progress`; when `waiting_for_input` is true, wait for the caller's pending reads and feed responses. Core never waits itself.
4. `Done(ScanOutput { snapshot, stats })` transfers the result once. Later calls are inactive.

`ScanProgress` reports the current phase, `records_processed`, `records_planned`, outstanding requests and whether input is needed. `records_planned` is `None` during boot, record-zero reading and discovery. From the first `Stream` report through completion it contains the MFT DATA size divided by the record size, including unused slots.

The [crate docs](src/lib.rs) contain a runnable in-memory driver loop. Bootstrap follows boot → record 0 → attribute-list extension discovery → MFT streaming. Records are assembled across response and extent boundaries; the caller never needs to buffer the whole raw MFT. Transport bounds do not bound owned metadata memory.

## Snapshot and errors

`node`, `children`, `name` and iterative `path` expose immutable hot/cold fields, pooled names and CSR edges. `volume`, `top_files` and `ext_stats` expose parsed/caller metadata and final indexes. Snapshot is Send + Sync; share it through `Arc` to avoid deep clones.

`resolve_path(path)` resolves a volume-relative path from the root, accepting `/` and `\` separators and skipping empty components. Exact names take precedence over Unicode lowercase matches; `.` and `..` are literal names. A component starting with `#` requires an ASCII decimal NodeId among the current directory's direct children. Empty paths resolve to the root, and the final node may be a file. Missing components, ambiguous matches with candidate NodeIds, and descent through a file return `QueryError`; callers check directory requirements after resolution.

Self logical size is unnamed DATA; ADS logical bytes are separate. Allocation includes every non-resident stream: ordinary streams use mapped clusters across all segments × cluster size, including preallocation; sparse/compressed flagged streams use first-segment `compressed_size`. Resident allocation is zero. Holes without an LCN, such as `$BadClus:$Bad`, contribute nothing. Primary links alone contribute to ancestors and file indexes. Output is deterministic across chunk sizes, caps and response orders; final name comparisons use UTF-16 re-encoding of pooled text.

`rank_files(under, &FileFilter, n)` ranks matching primary files within a subtree, including files beyond the stored top 100. `FileFilter::extensions` accepts alternative suffixes, drops one leading dot and lowercases ASCII letters; `""` matches names without a usable extension. `FileFilter::Kind` uses these category names: `video`, `audio`, `image`, `archive`, `disk-image`, `document`, `code`, `executable`, `other`. `FileKind::ALL`, `name()` and string parsing expose these names. The common extension table assigns `ts` to video and unknown suffixes to other.

`extension_stats_under(under)` returns subtree extension totals in the same order as `ext_stats`; the root scope copies the stored index. `kind_stats(under)` combines those totals into `KindStat` rows, ordered by allocation descending and then category order. Query scopes may be directories or individual files. Rankings and totals exclude duplicate hard links. Queries traverse iteratively, reserve storage fallibly and return `QueryError` for an unknown scope or a failed calculation/allocation.

`NamePattern::glob(text)` compiles a case-insensitive name glob, preserving the input through `as_str()`. Input without `*?[{` uses substring matching, so `steam` finds `SteamLibrary`. Globs match whole names, allow wildcards across separators and support backslash escaping. Input is limited to 256 bytes; invalid globs and oversized input return `QueryError`.

`search(under, &pattern, offset, limit)` visits descendants in pre-order using stored child order, including directories, synthetic directories and duplicate hard links. The scope node itself is excluded. `SearchPage::items()` contains at most `limit` matches; `total_matches()` counts every match, and `next_offset()` supplies the next page offset when more remain. Offsets count matches, and an offset past the end returns an empty page with the full total. A zero limit returns only the total and retains the offset if matches remain.

To query a saved snapshot from [adamite-cli](../adamite-cli/README.md), run `adamite top C.adm --under /Users --kind video`, `adamite ext C.adm --by kind`, or `adamite search C.adm '*.iso' --offset 0 --limit 50`.

`scan::ConfigError` rejects invalid bounds, `scan::FeedError` rejects responses without changing state, `error::ScanError` terminates a failed scan, and `snapshot::SnapshotError` covers snapshot construction/path failures. Ordinary corrupt records are discarded and counted; a missing/corrupt root or bootstrap is fatal. Errors preserve parser/allocation sources through snafu.

## Persistence

`scan::ScanOutput::to_bytes(&self)` returns `Result<Vec<u8>, snapshot::PersistError>`; `ScanOutput::from_bytes(&[u8])` returns `Result<ScanOutput, snapshot::PersistError>`. Both preserve the complete snapshot, caller context and scan counters. `ScanOutput::ADM_MAGIC` exposes the eight-byte signature for identifying snapshot files before decoding. The codec lives in private `snapshot/persist/`; callers own file reads and writes.

Both convenience methods use the bounded codec: `ScanOutput::encoder()` returns `snapshot::SnapshotEncoder`, which emits chunks of at most 1 MiB and exposes the final CRC patch after completion. Labels and name pools are borrowed; integer columns use reusable scratch. `snapshot::SnapshotDecoder::verify(file_len, copy)` verifies the first sequential pass before column allocation; after rewinding the unchanged source, `decode(copy)` reads the second pass into final storage. The fallible callback fills each requested byte slice in order and preserves caller errors through `From<PersistError>`. Shared file adapters are `adamite_io::snapshot::{SnapshotWriter, SnapshotReader}`.

Canonical `.adm` v1 uses little-endian fields, a 64-byte header with version, lengths and whole-file CRC-32, a 13-entry section table and 8-byte-aligned sections. Optional columns store a presence bitmap followed by a full value array. Accepted bytes re-encode identically.

Decode checks the header, table, placement, padding, lengths and CRC before fallible column allocation, then validates geometry, UTF-8 and all snapshot invariants, including tree connectivity, hard-link groups, aggregates, ordering and indexes. Scan counters are preserved without cross-validation against the tree. The non-exhaustive `snapshot::PersistError` reports format, checksum, invariant, geometry, UTF-8, overflow, allocation and encoding-width failures.

## Local checks and fixtures

From the repository root:

```text
deno task check
cargo test -p adamite-core --test fixtures --locked
PROPTEST_CASES=1024 cargo test -p adamite-core --test properties --locked
```

The shared `adamite-fixtures` crate loads committed images and independent v3 manifests. On Linux with ntfs-3g, zstd and attr installed, `deno task fixtures` regenerates both using sudo. Normal checks need no mounts or system NTFS tools. Fixture tests compare paths, metadata, stream allocation, subtree totals and indexes; property tests vary transport configurations and response order. Scanner unit tests are grouped by protocol, bootstrap, discovery, stream, merge, link, snapshot and done phases, with shared builders/drivers in `scan/scanner/tests/support.rs`. Other unit tests stay beside their owners; `tests/thread_safety.rs` checks public trait bounds, cross-thread scanning and concurrent shared-snapshot reads.

## Fuzzing and future work

The [standalone nightly fuzz harnesses](fuzz/README.md) check bounded scan inputs, protocol and aggregation invariants, plus `.adm` decoding and canonical re-encoding. `deno task fuzz-corpus` generates parser seeds and both core corpora; `deno task check --fuzz` runs all seven smoke targets. Smoke mutations go to scratch directories under `target/fuzz-smoke`; committed seed corpora are read without changes.

[`adamite-io`](../adamite-io/README.md) provides `VolumeReader`, slice/image/Windows raw-volume readers and `SyncDriver` for synchronous scans. [`adamite-cli`](../adamite-cli/README.md) scans and saves `.adm` files, prints reports and launches the [`adamite-tui`](../adamite-tui/README.md) browser. Parallelism, buffer pools, `$BITMAP` skipping, general resource limits, MCP, USN and walkdir fallback remain future changes. Mirror fallback, reparse-target traversal and atomic live-volume snapshots are not provided.
