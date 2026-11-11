# Plan: snapshot persistence (.adm)

## Why

- ARCHITECTURE §4.1's MVP is "scan once elevated, query many times unelevated": `adamite scan C: -o C.adm` writes a snapshot file that later processes read without device access. No serialized form of `ScanOutput` exists, so every query would have to rescan the volume.
- `Snapshot` storage is private to the snapshot module family (`SnapshotStorage` is `pub(super)`), so only core can rebuild a snapshot from bytes. `VolumeGeometry` can only be built from a boot sector.
- A saved file is read by processes that did not create it. Decoding must treat it as untrusted input: every public accessor on a decoded snapshot must behave exactly as on a scanned one, without panics, unbounded allocation or loops.

## What Changes

- `adamite-ntfs`: a checked field-wise `VolumeGeometry` constructor sharing boot-sector validation.
- `adamite-core`: a canonical, versioned `.adm` v1 byte format for a complete `ScanOutput` (snapshot and `ScanStats`), with `ScanOutput::to_bytes` / `ScanOutput::from_bytes`, full structural and semantic validation on decode, a typed `PersistError`, round-trip/mutation tests and a `persist` fuzz target.
- Fuzz corpus generation, the local gate, CI fuzz matrix, an elevated real-volume round trip and documentation follow.

## Scope

In scope: the two crates above, the core fuzz package, `scripts/{check,check_test,fuzz_corpus}.ts`, `deno.json` permissions, `.github/workflows/ci.yml` fuzz matrix, `crates/adamite-io/tests/raw_volume.rs`, ARCHITECTURE and READMEs.

Out of scope: file I/O (callers read/write files; core stays I/O-free), memory-mapped or borrowed snapshot views, format migration (only v1 exists), compression, queries, CLI (see [cli](cli.md)).

Follow the project's Rust best practices and existing conventions: core forbids unsafe, keeps its panic-free Clippy denies, uses checked arithmetic and fallible allocation; one public path per item, no root re-exports, operations belong to types; no test-only fields; no `cfg(test)` staging of production code or lint suppression to keep milestones green; no bash/Python scripts. Existing fixtures, the scan fuzz target, all existing corpus files and the six existing smoke targets stay unchanged.

## Impact

- `crates/adamite-ntfs/src/boot.rs` (+ tests): new field-input type, constructor and error variant. `no_std` build unchanged.
- `crates/adamite-core`: new private `snapshot/persist/` and `snapshot/rules.rs`, `snapshot::PersistError` export, `ScanOutput` methods, dependency `crc32fast` (MIT OR Apache-2.0, `default-features = false`, `std`). Both `Cargo.lock` and `crates/adamite-core/fuzz/Cargo.lock` update in the milestone adding it.
- `crates/adamite-core/fuzz`: `persist` target, `persist_seeds` example, harness test, README; corpus under `fuzz/corpus/persist/`.
- `scripts/check.ts` (`fuzzPackages`), `scripts/check_test.ts` (inventory counts and the fuzz-corpus permission assertion), `scripts/fuzz_corpus.ts`, `deno.json` (`fuzz-corpus` gains `cargo` in `--allow-run`), CI fuzz matrix.
- `crates/adamite-io/tests/raw_volume.rs`: the elevated C: scan also round-trips through `.adm` bytes.
- ARCHITECTURE §1 (persistence implemented inside `snapshot/`; new dependency edge), §3, §4 intro, §6, §7; `docs/plans/core-layout.md` is historical and stays unchanged, so the dependency table update lives in ARCHITECTURE; core and fuzz READMEs.

## Design decisions

1. D1. API and placement. The codec lives in private `snapshot/persist/` (`mod.rs`, `format.rs` header/section table/CRC, `encode.rs`, `decode.rs`, `validate.rs`, `error.rs`); a top-level `persist` module would require widening storage visibility. Its crate-visible boundary encodes `(&Snapshot, &ScanStats)` and decodes to `(Snapshot, ScanStats)`. Snapshot persistence may depend on the independent scan leaves `ScanContext` (already used by storage) and `ScanStats`, never on `ScanOutput`, status, protocol or scanner. Public surface, implemented in `scan/status.rs`, which assembles the result:

   ```rust
   impl ScanOutput {
       pub fn to_bytes(&self) -> Result<Vec<u8>, PersistError>;
       pub fn from_bytes(bytes: &[u8]) -> Result<Self, PersistError>;
   }
   ```

   `PersistError` is exported only as `snapshot::PersistError`. Core takes byte slices and returns owned buffers: no `io::Read`/`Write`, no file paths. ARCHITECTURE's planned top-level `persist/` box is replaced by this placement, and its dependency description gains `snapshot::persist → scan::{config, stats}`.

2. D2. `.adm` v1 layout. Little-endian, fixed field widths, independent of Rust memory layout (never copy `repr(C)` structs or `Option` representations). The encoding is canonical: every accepted file is the unique encoding of its decoded value, so `to_bytes(from_bytes(b)) == b`.
   - Header, 64 bytes: magic `b"ADAMITE\0"` (8), `format_version: u32 = 1`, `header_len: u32 = 64`, `file_len: u64`, `section_count: u32 = 13`, `flags: u32 = 0`, `crc32: u32` (bytes 32..36), `reserved: [u8; 28] = 0`.
   - Section table at byte 64: 13 entries of 32 bytes (`kind: u32`, `flags: u32 = 0`, `offset: u64`, `len: u64`, `count: u64`), ending at byte 480. Kinds 1–13 in this order: `Volume`, `Stats`, `Nodes`, `Names`, `Children`, `AdsSize`, `FileRefs`, `Attrs`, `Nlink`, `ReparseTags`, `TopFiles`, `ExtStats`, `ExtNames`; each occurs exactly once.
   - Placement: the first section starts at 480; every later section starts at exactly `align_up(previous.offset + previous.len, 8)`; padding is the minimum needed and all zero. `file_len` equals the end of the last section (no final padding).
   - Counts: `Volume.count = 1`, `Stats.count = 26`, byte pools' counts equal their byte lengths, arrays' counts equal their element counts.
   - Fixed-width arrays (`len = count × width`): `Nodes` 64 bytes each in field order `size, alloc, total_size, total_alloc, mtime` (u64), `parent, child_start, child_len, file_count, name_off` (u32), `name_len, flags` (u16); `Children`/`TopFiles` u32 node ids; `AdsSize` u64; `Attrs` u32; `Nlink` u16; `ExtStats` 32 bytes each: `count, total_size, total_alloc` (u64), `name_off, name_len` (u32).
   - Optional columns (`FileRefs` u64 packed `FileRef`, `ReparseTags` u32), `n` entries: a presence bitmap of `(n + 7) / 8` bytes (LSB-first, unused bits zero), then a full value array of exactly `n` entries starting at section-relative `align_up(bitmap_len, 8)`; section length is exactly `value_offset + n × width`; internal padding and absent values are zero.
   - Pools: `Names` is the snapshot name pool (D4 requires it tightly packed in node order). `ExtNames` concatenates extension strings in `ExtStats` order: each `name_off` equals the previous end and the last end equals the pool length (empty extensions take zero bytes).
   - `Volume`, packed without internal padding: geometry 46 bytes (`bytes_per_sector` u16, `sectors_per_cluster` u32, `file_record_size` u32, `index_record_size` u32, `total_sectors` u64, `mft_lcn` u64, `mftmirr_lcn` u64, `serial` u64; derived values are recomputed); `scanned_at_filetime` and `free_bytes` each a tag byte (0 or 1) plus an always-present u64 (zero when absent); `label` a tag byte, an always-present u32 byte length (zero when absent) and that many UTF-8 bytes. A present empty label stays distinct from absence. Section length is exactly `69 + label_len`.
   - `Stats`: exactly 26 u64 counters (208 bytes) in the v1 order listed in the format module's documentation, from `records_total` through `metafiles` (the current declaration order). Reordering Rust fields does not change v1; adding a persisted counter requires a new format version. Counters are not cross-validated against the snapshot.
   - CRC-32 (IEEE, `crc32fast`) covers the whole file with bytes 32..36 read as zero.

3. D3. Decode pipeline, cheapest checks first and before any allocation proportional to declared counts: magic, version, `header_len`, `section_count`, header flags and reserved bytes; `file_len == bytes.len()`; table kinds, order, flags, exact placement, padding, lengths per D2's formulas (counts are derived from and checked against actual bytes, never trusted for allocation); CRC. Then fallible column allocation and decoding, then D4 validation, then `Snapshot::from_storage`. Unknown versions, kinds or flag bits are rejected (no forward compatibility in v1).

4. D4. Validation. A decoded `ScanOutput` must satisfy every invariant the snapshot build passes and the linker establish, so accessors return the same results as for a scanned snapshot, and must accept every snapshot a scan can produce (a rejected legitimate snapshot is a round-trip bug). Structural and aggregate checks are iterative, use checked arithmetic and run in `O(n + e + B)`; extension checks add `O((n + k) log(k + 1))` comparisons plus string-comparison cost.
   - Columns: `1 <= n <= u32::MAX`; every cold column has length `n`; the name pool fits `u32`.
   - Root: id 0, parent 0, name `/`, DIRECTORY, FileRef record 5, not SYNTHETIC or HARDLINK_DUP.
   - Arena order: at most one `<NTFS metadata>` and one `<orphaned>` synthetic node, immediately after the root in that order; synthetic nodes have exactly `DIRECTORY | SYNTHETIC`, parent root, zero self size/alloc/ADS/mtime/attrs/nlink, no FileRef and no reparse tag (subtree totals may be non-zero). The remaining nodes form contiguous FileRef-record groups in ascending record order, excluding record 5; each group's first node is its only node without HARDLINK_DUP (directories included). Within a group, size, alloc, ads_size, mtime, attrs, nlink, reparse tag, FileRef sequence and flags other than HARDLINK_DUP are equal; parent, name, CSR fields and totals may differ. Names alone never identify synthetic nodes.
   - CSR: `children.len() == n - 1`; ranges partition `children` in arena order; every non-root id appears exactly once; `child.parent` equals the owning node; parents are directories without HARDLINK_DUP; non-directories and HARDLINK_DUP nodes have no children.
   - Connectivity: an iterative traversal from the root visits all `n` nodes exactly once.
   - Names: ranges in bounds, valid UTF-8, tightly packed in node order covering the pool exactly.
   - Flags: only the eight known bits. HIDDEN and SYSTEM equal their attribute bits. REPARSE equals attribute REPARSE_POINT or a present reparse tag. Attribute DIRECTORY, SPARSE_FILE and COMPRESSED each imply the corresponding flag; the reverse is not required (those flags may come from record or stream flags that are not persisted). Flags are never re-derived from attributes alone.
   - Aggregates: for every node, `total_size = size + Σ`, `total_alloc = alloc + Σ`, `file_count = is_primary_file + Σ`, summing direct children without HARDLINK_DUP.
   - Child order within each range: `total_alloc` descending, then pooled name compared as UTF-16 text, then optional `(record, sequence)`, then NodeId. `top_files`: exactly the top `min(100, primary files)` by `alloc` descending with the same tie-breakers, verified with a bounded selection pass (no full sort of `n`). `ext_stats`: recomputed from primary files with the finalizer's ASCII-only extension folding and grouping, compared for equality, in its stored order.
   - Volume: geometry via D5; label UTF-8. `ScanContext` values and `ScanStats` counters are otherwise unconstrained. Ranking (comparators, top-file selection) and extension extraction move from private build modules into a snapshot-private `snapshot/rules.rs`, used by both finalization and validation so they cannot drift. Flag derivation stays in MFT; validation uses only the snapshot-local relations above.

5. D5. Geometry. `adamite_ntfs::boot` gains `GeometryFields` (public fields: the eight independent values) and `VolumeGeometry::from_fields(GeometryFields) -> Result<VolumeGeometry, BootError>`, both directly under `boot`. Validation shared with `from_boot_sector`: sector bytes in {512, 1024, 2048, 4096}; sectors per cluster a power of two in 1..=4096 with checked cluster bytes <= 2 MiB; record sizes powers of two in 512..=65536; total sectors in 1..=`i64::MAX`, checked total bytes and non-zero floor-divided total clusters; room for one file record at both MFT locations within complete clusters. `from_boot_sector` keeps its existing validation order and error fields (its record-size errors still carry the raw encoding). Invalid field inputs use a new documented `BootError::InvalidField { field: GeometryField, value: u64 }`, where `GeometryField` is a fieldless `#[non_exhaustive]` enum naming the validated inputs (a typed field keeps `BootError` within its existing 24-byte size test). `VolumeGeometry::fields()` returns the `GeometryFields` of a geometry for encoding. No new conditions beyond the parser's.

6. D6. Errors. `snapshot::PersistError`: one `#[non_exhaustive]` snafu enum, `Send + Sync`, separate from `SnapshotError`, with `&'static str` reasons: `BadMagic`, `UnsupportedVersion { version }`, `Truncated { needed, available }`, `InvalidHeader { reason }`, `InvalidSectionTable { reason }`, `ChecksumMismatch { expected, actual }`, `InvalidSection { section, reason }`, `InvalidTree { reason, node }`, `InvalidIndex { reason }`, `Geometry { source: BootError }`, `Utf8 { section, source }`, `Overflow { operation }`, `Allocation { source }`, `TooLarge { operation }` (encoding values that exceed format widths).

7. D7. Tests.
   - Round trip: all three fixtures scanned; `to_bytes` → `from_bytes` equals the original `ScanOutput`; two encodings are byte-identical; `to_bytes(from_bytes(b)) == b`; `ScanContext` with all, none and an empty present label round-trips.
   - A committed golden v1 encoding of a small synthetic `ScanOutput` (hex in the test) pins the byte format.
   - Rejections: each header field, truncation at every section boundary, extra padding, misplaced sections, CRC mismatch, version and unknown kind; one targeted test per D4 rule (mutate decoded columns of a valid file, re-encode with a test helper that recomputes the CRC, expect the specific variant). Proptest: random byte mutations with CRC repair never panic and either fail or round-trip byte-identically.
   - Geometry: `from_fields` equals `from_boot_sector` on all fixtures; boundary rejections.
   - Real volume: the elevated `adamite-io` C: test asserts `from_bytes(to_bytes(out)) == out`, the only live-volume check of D4 before the CLI exists.

8. D8. Fuzz. New `adamite-core/fuzz` target `persist`: decode arbitrary bytes; on success visit every node through all accessors (bounded `path` calls), re-encode and assert byte equality; a second branch repairs the CRC before decoding so mutations reach semantic validation. Seeds come from a fuzz-package example run as `cargo run --manifest-path crates/adamite-core/fuzz/Cargo.toml --example persist_seeds --locked -- <out>`, which scans small synthetic images (built with `adamite-fixtures` builders) and reduced fixture-derived images, then writes encoded files plus truncated and corrupted variants within per-seed and total budgets compatible with the existing `-max_len=262144` smoke flags. Full-fixture round trips stay in normal tests. `deno task fuzz-corpus` (default and `--core` modes) generates both core corpora, keeping existing scan corpus bytes unchanged; its permissions become `--allow-run=cargo,zstd` and `check_test.ts`'s assertion follows. `fuzzPackages`, the CI fuzz matrix and the inventory assertions add `adamite-core/persist`; hard-coded target counts (6 total, 1 core) are derived from the inventory instead.

## Tasks

Each milestone ends with one reviewed commit and `deno task check` green. A milestone may use more than one bounded Codex run (< 40 minutes each) before that commit.

### P1 — Field-wise geometry

- [x] Add `GeometryFields`, `VolumeGeometry::from_fields` and `BootError::InvalidField` per D5, sharing validation with `from_boot_sector`. Tests per D7; `no_std` build stays green.

### P2 — Shared rules, format and encoder

- [x] Move ranking and extension extraction into `snapshot/rules.rs`, used by finalization (behaviour unchanged; existing tests pass). Add `crc32fast` (both lockfiles), `snapshot/persist/{mod,format,encode,error}.rs`, `PersistError` and public `ScanOutput::to_bytes` per D1, D2, D6. Determinism and golden-encoding tests.

### P3 — Decoder and validation

- [x] Add `decode.rs` and `validate.rs` with every D3 and D4 check and public `ScanOutput::from_bytes`. Round-trip tests on all fixtures and the context variants.

### P4 — Rejection tests

- [x] Add the D7 header/section/CRC rejection tests, one targeted mutation test per D4 rule and the CRC-repairing proptest.

### P5 — Fuzzing and gate

- [x] Add the `persist` target, harness test, `persist_seeds` example and corpus generation per D8; update `fuzzPackages`, CI matrix, `check_test.ts`, `deno.json`. Finish with `deno task check --fuzz` green.

### P6 — Real-volume round trip and documentation

- [ ] Extend `adamite-io/tests/raw_volume.rs` per D7; update ARCHITECTURE, core and fuzz READMEs. After the commit, confirm the elevated Windows CI round trip passes.

## Acceptance criteria

- [ ] All three fixtures round-trip through `ScanOutput::{to_bytes, from_bytes}` with equality and byte-identical re-encoding; the golden v1 test passes.
- [ ] Every D4 rule has a targeted rejection test returning the specified `PersistError` variant; no test relies on panics.
- [ ] `rg -n 'unsafe' crates/adamite-core/src` matches only the crate-level `forbid`; `rg -n 'std::fs|File::' crates/adamite-core/src` has no matches.
- [ ] `deno task check` and `deno task check --fuzz` pass with seven smoke targets; the existing six corpora are byte-identical and `fuzz/corpus/persist` is non-empty.
- [ ] CI `check (windows-latest)` passes the elevated C: scan with `.adm` round trip.
- [ ] ARCHITECTURE and READMEs describe the implemented format location, dependency edge and API.
