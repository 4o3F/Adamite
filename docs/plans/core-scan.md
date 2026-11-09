# Plan: adamite-core single-threaded sans-io scan

## Why

Adamite can decode NTFS structures, but it cannot yet turn them into a complete file tree with reliable size totals. Implementing the single-threaded scan in docs/ARCHITECTURE.md §2–§3 and §6 step 2, with caller-supplied bytes and independent fixture oracles, establishes the scan behavior before platform I/O and parallel execution are added.

## What Changes

- Add `crates/adamite-core`, a `std` library covering scan phases 0–6 with a sans-io state machine. Core performs no file, device or network I/O, defines no reader trait or callback, and creates no threads or sleeps.
- Expose `step()` returning `Need(ReadRequest)`, `Progress` or `Done(ScanOutput { Snapshot, ScanStats })`, with `feed(id, bytes)` matching responses to a `RequestId`. Requests describe volume-relative byte ranges. The API supports multiple outstanding requests with a configurable cap, defaulting to 1. Scan input streams in bounded chunks, without requiring the caller to hold the whole MFT, and records crossing extent boundaries are reassembled, including `frag-512` record 4095.
- Bootstrap `$MFT` from record 0's own runlist, including `$ATTRIBUTE_LIST` discovery of extension segments reachable through known extents. Unresolvable discovery terminates with a no-progress error.
- Ingest records and merge extension contributions using `Patch` values carrying the complete base `FileRef` and per-stream segment identity. Non-resident sizes come only from the segment with `lowest_vcn == 0`. Deleted, `BAAD` and unused records are excluded from the tree and counted in `ScanStats`, alongside parser failure categories.
- Apply the architecture's accounting rules: `size` is unnamed `$DATA` logical size; ADS logical bytes are separate in `ads_size`; `alloc` includes all non-resident streams, including ADS and `$INDEX_ALLOCATION`, with `compressed_size` used for compressed/sparse storage and zero allocation for resident values. Exclude `$BadClus:$Bad` logical size.
- Build parent links with sequence validation and DOS alias suppression. Keep every hard link as a node, including distinct names under the same parent. The smallest `(parent FRN, name)` selects the primary link for aggregation; other links carry `HARDLINK_DUP`. Place orphans and cycles under `<orphaned>`, and records 0–15 plus `$Extend` children under `<NTFS metadata>`.
- Finalize a flat tree with a compressed sparse row (CSR) child index, iterative aggregation, children sorted by descending `total_alloc` with a stable tie-break, `top_files` and `ext_stats`. Produce a deterministic, immutable `Snapshot` that is `Send + Sync` and shareable through `Arc`, with minimal read accessors for node, children, name and path. Keep an `EntryRecord`-like boundary between NTFS phases 0–2 and generic tree phases 3–6.
- Extend the Deno fixture generator to schema v3, recording independently measured per-stream `allocated_size`/`compressed_size` and metafile entries through `ntfsinfo`. The user will regenerate fixtures with `deno task fixtures` using sudo. Both parser and core tests consume the regenerated images and manifests; existing parser tests continue to pass with v3.
- Move fixture loading into a shared, development-only, `publish = false` crate, `crates/adamite-fixtures`, used by parser and core tests without depending on core logic. Core integration tests serve requests from decompressed image bytes and compare the completed scan against the independent manifests.
- Adopt the parser's strict panic-free Clippy set and snafu errors. Add `tracing` as a workspace dependency, with core spans per phase, and a full-scan cargo-fuzz target checking panic freedom and bounded read requests.
- Correct docs/ARCHITECTURE.md to describe the sans-io boundary where the pipeline currently couples core to `VolumeReader`, read execution and buffer pools. Require a compile-time size assertion for the finalized `Node` layout.

Out of scope: `adamite-io`/`VolumeReader` belongs to the next `add-volume-reader` change; parallelism, buffer pools, `$BITMAP` skipping and resource limits remain roadmap step 3. Query operations such as `summarize`, `list_dir` and `search` belong to `add-core-query`. Persistence, CLI, MCP, USN refresh and the walkdir fallback are deferred. Automated WizTree/`dir /s` comparison is excluded; manual Windows comparison follows later.

## Scope areas


- `core/scan-protocol`: Drives scans through identified byte-range requests, supplied responses, progress and completion, with capped outstanding requests, bounded input chunks and explicit protocol errors.
- `core/mft-bootstrap`: Discovers and validates the complete MFT mapping from boot bytes, record 0 and attribute-list extension references, including fragmented records and no-progress termination.
- `core/record-merge`: Classifies records, merges extension segments by file and stream identity, applies logical/allocation accounting, and produces scan entries and diagnostic counts.
- `core/tree-linking`: Resolves parent identities, preserves hard links, selects deterministic primary links, suppresses DOS aliases and groups orphaned/cyclic entries and NTFS metadata.
- `core/aggregation`: Computes subtree totals without recursion, counts primary links once, and produces deterministic child ordering, top files and extension statistics.
- `core/snapshot`: Publishes the immutable flat tree, CSR children, names and metadata with minimal read accessors, thread-sharing guarantees and an asserted node layout.


## Impact

- **Code and APIs:** new `crates/adamite-core/**` and `crates/adamite-fixtures/**`; parser test support migrates to the shared fixture crate. Existing NTFS decoding APIs remain sufficient.
- **Dependencies:** core uses `adamite-ntfs`, `snafu` and workspace `tracing`; shared fixture support uses `ruzstd`, `serde` and `serde_json`; full-scan fuzzing uses `libfuzzer-sys`.
- **Fixtures:** `scripts/fixtures/**` and `tests/fixtures/ntfs/**` gain schema v3 allocation and metafile oracles. Fixture generation requires Linux tools and sudo; normal tests read committed images on Linux and Windows without mounting them.
- **Validation:** workspace checks and CI cover the new crates, schema v3 compatibility, full-scan fixture tests and fuzz smoke coverage while retaining parser `no_std` checks.
- **Docs and downstream:** docs/ARCHITECTURE.md gains the corrected core/I/O boundary and node-layout requirement. Later I/O adapters fulfill read requests; query consumers build on the immutable snapshot.
