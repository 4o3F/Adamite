# Plan: bound scan memory and I/O

## Why

- Scanning a Windows system volume peaked at about 2 GB RAM and 600 MB/s of sustained reads; on weak machines this can stall the whole system.
- A synthetic 1.25M-slot MFT (1M records in use) reproduces it on Linux: 1,275 MiB peak RSS while the finished snapshot holds 133 MiB. Ingest alone requests 1,163 MiB: per-record stream (120 B × capacity 4) and standard-information (72 B × capacity 4) vectors, a 128-byte record slot array rounded to a power-of-two capacity, and name buffers. Linking and snapshot construction overlap old and new arrays within their stages; `scan -o` holds the snapshot and the whole encoded file, and loading holds the whole file while decoding.
- Reads cover the whole MFT in 1 MiB requests with nothing pacing them, so the disk runs at full sequential speed for the duration.

The investigation report is kept in `.ccg/tasks/scan-resource-usage/analysis-codex.md`.

## What Changes

- Adamite limits its own resources; it does not rely on operating-system priority classes or background modes.
- Core keeps far less per-record state and frees each stage's inputs as soon as they are consumed.
- `adamite-io` gains `ScanLimits`, enforced by `SyncDriver`: a read-rate budget it paces itself, a native I/O depth and chunk size, and the thread count for future parallel work (1 today). The CLI exposes them through profiles and overrides.
- `.adm` encoding and decoding no longer need a second full-size buffer.
- Every performance-relevant path is instrumented with `tracing` spans, and any `adamite` invocation can write a Chrome trace (`tracing-chrome`) with heap usage attached, so real volumes — including a raw C: scan on Windows — can be profiled, not only synthetic ones.
- A development benchmark crate reproduces large synthetic volumes with the same profiling; a small-scale heap-budget test guards regressions in the gate.

## Scope

In scope: tracing instrumentation across core, io, persistence and the CLI; the `adamite-profile` crate and the CLI `--trace-chrome` option; core ingest/merge/link/build memory, `ScanLimits` in `adamite-io`, CLI flags, `.adm` streaming, the benchmark crate and budget test, ARCHITECTURE §5/§7 and READMEs.

Out of scope: `$MFT:$BITMAP` skipping, parallel parsing (threads stay 1; the limit is plumbed for later), OS priority APIs (`PROCESS_MODE_BACKGROUND_BEGIN`, `SetThreadPriority`, `FILE_FLAG` priority hints), changing the `.adm` format.

## Design decisions

1. D1. Self-limiting only. `ScanLimits` replaces ARCHITECTURE §5's `low_priority` field and OS-priority text. Every limit is enforced by adamite code: pacing sleeps in the driver, capped request depth and chunk size, bounded buffers, and (later) a bounded worker pool.
2. D2. Instrumentation and profiling.
   - Spans everywhere performance matters, at stage or batch granularity (never per record or per byte, so disabled tracing costs nothing measurable): core keeps its per-call phase spans and adds spans with counts for each ingest/merge/link/build/finalize stage; `adamite-io` adds spans for `SyncDriver` batches (bytes, requests), reader reads and rate-limit waits; core persistence adds encode/decode/validate spans; the CLI adds spans for source detection, snapshot load, scan, save and rendering; the TUI adds spans for startup and frame rendering.
   - New unpublished library crate `adamite-profile` (same lints) owns the shared profiling pieces so they are not duplicated: a counting global allocator type (`CountingAllocator`, relaxed atomics for current/peak requested bytes), a `tracing` layer that records heap current/peak and the delta on every span close as span fields, and a guard that installs a `tracing-chrome` `ChromeLayer` writing to a file. Libraries (`adamite-core`, `adamite-io`, `adamite-tui`) only emit spans and never depend on it.
   - CLI: the binary registers `CountingAllocator` as its global allocator and gains a global `--trace-chrome <FILE>` option that installs the profiling subscriber for that run (no subscriber otherwise, so ARCHITECTURE §7 changes from "installs no subscriber" to "installs one only on request"). The trace opens in `chrome://tracing` or Perfetto and shows each stage's duration, bytes read and heap.
   - `adamite-bench` (binary, `publish = false`, same lints, depends on `adamite-profile`): generates sparse synthetic images with `adamite-fixtures` builders (slot count, in-use ratio, directory/file mix, name length, DOS aliases, resident/non-resident DATA), runs scan, `scan -o`-equivalent encode and load, and prints a table of bytes read, elapsed time and peak heap overall and per stage, optionally with `--trace-chrome`. Run via `deno task bench`, not part of `deno task check`.
3. D3. Regression budget. A gate test in `adamite-bench` on a small synthetic volume (about 50k slots, so it stays fast) asserts peak requested heap per MFT slot and snapshot bytes per node stay below the budgets set after M2/M3, read from the `adamite-profile` counters. Requested-heap counts are deterministic, unlike RSS.
4. D4. Ingest compaction (core). Size per-record containers exactly from the attributes actually decoded (no default capacity 4; one standard-information entry stored inline, not in a vector), shrink the record slot to the fields still needed after decode, and reserve the slot array exactly from the planned record count instead of power-of-two growth. Target: ingest peak ≤ 450 B per in-use record on the D2 reference mix.
5. D5. Stage overlap (core). Drop exhausted iterators, merged slots and replaced entry arrays before allocating their successors; build nodes in place where a second arena only re-sorts; release source node storage before freezing columns. Target: overall scan peak ≤ 500 MB per 1M in-use records on the reference mix, with the finished snapshot unchanged (all oracles and the golden `.adm` stay byte-identical).
6. D6. `ScanLimits` (`adamite-io`). Fields: `max_read_bytes_per_sec: Option<NonZeroU64>` (token bucket in `SyncDriver`, sleeping between requests; core still never sleeps), `io_depth: NonZeroUsize` (1 for the synchronous reader), `chunk_size` (feeds `ScanConfig.chunk_size`), `threads: NonZeroUsize` (validated, 1 today). Profiles: `Background` (64 MiB/s, 1 MiB chunks), `Balanced` (default, 256 MiB/s, 1 MiB chunks), `Max` (no rate cap, 4 MiB chunks). CLI: `--profile background|balanced|max`, `--max-read-rate <BYTES>[K|M|G]` and `--threads`, accepted by every command that can scan.
7. D7. `.adm` without double buffering (core + CLI). Encoding becomes a sans-io chunk producer (`ScanOutput::encoder()` yielding byte slices section by section with the CRC computed incrementally and written last into the header slot), so the CLI writes chunks to the temporary file without materialising the whole encoding. Decoding keeps full validation before construction: the CLI reads the file in fixed-size blocks into a decoder that verifies header, table and CRC in a first pass and decodes columns in a second pass from the same file, so no full-file buffer is held alongside the snapshot. The byte format and every rejection rule are unchanged.
8. D8. Tests and measurements. Each milestone reports D2 numbers (before/after) for 300k and 1M in-use records in its review. Core oracles, the golden `.adm`, persist corpus and fuzz targets must stay green and byte-identical except where a milestone explicitly says otherwise. `ScanLimits`: unit tests of the token bucket with an injected clock, driver tests that request sizes and depth are honoured, CLI parsing tests.

## Tasks

Each milestone ends with one reviewed commit, `deno task check` green and CI green; each Codex run stays under 40 minutes.

### M1 — Instrumentation, profiling and baseline

- [x] Add the D2 spans across core, io, persistence, CLI and TUI; add `adamite-profile`, the CLI `--trace-chrome` option and counting allocator, `adamite-bench` and `deno task bench`; record the baseline for 300k and 1M in-use records in the commit message.

### M2 — Compact ingest state

- [x] Implement D4; report D2 numbers.

### M3 — Stage overlap

- [x] Implement D5; add the D3 budget test with budgets from the measured result; report D2 numbers.

### M4 — Self-imposed limits

- [x] Implement D6 in `adamite-io` and the CLI; update ARCHITECTURE §5 per D1.

### M5 — Streaming `.adm`

- [x] Implement D7; report peak memory for `scan -o` and `info <file>.adm` on 1M records.

## Acceptance criteria

- [x] On the 1M in-use reference volume, scan peak requested heap ≤ 500 MB and `scan -o` / `info` add no full-file buffer.
- [x] With the default profile, reads never exceed 256 MiB/s averaged over any second; `--profile max` restores unthrottled reads.
- [x] Snapshot contents, the golden `.adm`, corpora and all oracles are unchanged.
- [ ] `adamite --trace-chrome trace.json scan <source>` writes a trace showing every D2 stage with duration and heap, on Linux images and a Windows drive.
- [ ] `deno task check` (with the budget test) and CI pass; ARCHITECTURE §5/§7 and READMEs describe the self-imposed limits and profiling.

