# adamite-core

`adamite-core` scans caller-supplied NTFS bytes into an immutable `snapshot::Snapshot` and `scan::ScanStats`. It uses `std`, forbids unsafe code and depends on `adamite-ntfs`, snafu and tracing. It performs no I/O, defines no reader trait or callback, and starts no threads or timers.

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

## Local checks and fixtures

From the repository root:

```text
deno task check
cargo test -p adamite-core --test fixtures --locked
PROPTEST_CASES=1024 cargo test -p adamite-core --test properties --locked
```

The shared `adamite-fixtures` crate loads committed images and independent v3 manifests. On Linux with ntfs-3g, zstd and attr installed, `deno task fixtures` regenerates both using sudo. Normal checks need no mounts or system NTFS tools. Fixture tests compare paths, metadata, stream allocation, subtree totals and indexes; property tests vary transport configurations and response order.

## Fuzzing and future work

The [standalone nightly scan harness](fuzz/README.md) bounds images, advertised metadata and driver calls while checking protocol and aggregation invariants. `deno task fuzz-corpus` generates both parser/core seeds; `deno task check --fuzz` runs all six smoke targets. Restore committed corpora after exploratory fuzzing.

Next is `adamite-io`/`add-volume-reader`. Parallelism, buffer pools, `$BITMAP` skipping, general resource limits, query operations (`add-core-query`), persistence, CLI, MCP, USN and walkdir fallback remain future changes. Mirror fallback, reparse-target traversal and atomic live-volume snapshots are not provided.
