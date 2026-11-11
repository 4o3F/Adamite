# adamite-core

`adamite-core` scans caller-supplied NTFS bytes into an immutable `snapshot::Snapshot` and `scan::ScanStats`. It uses `std`, forbids unsafe code and depends on `adamite-ntfs`, crc32fast, snafu and tracing. It performs no I/O, defines no reader trait or callback, and starts no threads or timers.

The public API lives under `scan::`, `snapshot::` and `error::`; the crate root exports modules only. Every scan item has one path directly under `scan::`, and its implementation children are private.

```text
src/
  lib.rs            crate docs, attributes and module declarations
  scan/             configuration, request protocol, status, stats and driver
  mft/              private bootstrap, discovery, mapping, ingest/decode and merge
  graph/            private entry model and parent/name/hard-link resolution
  snapshot/         immutable nodes, pooled names, indexes, volume and errors
    build/          private input, builder, aggregation and finalization passes
    persist/        private canonical .adm v1 encoder, decoder and validation
    rules.rs        private ranking and extension rules shared by build/persist
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

The [crate docs](src/lib.rs) contain a runnable in-memory driver loop. Bootstrap follows boot → record 0 → attribute-list extension discovery → MFT streaming. Records are assembled across response and extent boundaries; the caller never needs to buffer the whole raw MFT. Transport bounds do not bound owned metadata memory.

## Snapshot and errors

`node`, `children`, `name` and iterative `path` expose immutable hot/cold fields, pooled names and CSR edges. `volume`, `top_files` and `ext_stats` expose parsed/caller metadata and final indexes. Snapshot is Send + Sync; share it through `Arc` to avoid deep clones.

Self logical size is unnamed DATA; ADS logical bytes are separate. Allocation includes every non-resident stream, using compressed size for compressed/sparse streams. Primary links alone contribute to ancestors and file indexes. Output is deterministic across chunk sizes, caps and response orders; final name comparisons use UTF-16 re-encoding of pooled text.

`scan::ConfigError` rejects invalid bounds, `scan::FeedError` rejects responses without changing state, `error::ScanError` terminates a failed scan, and `snapshot::SnapshotError` covers snapshot construction/path failures. Ordinary corrupt records are discarded and counted; a missing/corrupt root or bootstrap is fatal. Errors preserve parser/allocation sources through snafu.

## Persistence

`scan::ScanOutput::to_bytes(&self)` returns `Result<Vec<u8>, snapshot::PersistError>`; `ScanOutput::from_bytes(&[u8])` returns `Result<ScanOutput, snapshot::PersistError>`. Both preserve the complete snapshot, caller context and scan counters. The codec lives in private `snapshot/persist/`; callers own file reads and writes.

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

[`adamite-io`](../adamite-io/README.md) provides `VolumeReader`, slice/image/Windows raw-volume readers and `SyncDriver` for synchronous scans. Parallelism, buffer pools, `$BITMAP` skipping, general resource limits, query operations (`add-core-query`), CLI, MCP, USN and walkdir fallback remain future changes. Mirror fallback, reparse-target traversal and atomic live-volume snapshots are not provided.
