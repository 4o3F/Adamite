# Plan: adamite-core single-threaded sans-io scan

## Why

Adamite decodes NTFS structures but cannot yet produce a complete file tree with reliable size totals. Implement phases 0–6 of docs/ARCHITECTURE.md using caller-supplied bytes and independent fixture oracles before adding platform I/O or parallel execution.

## What Changes

- Add `crates/adamite-core`, a single-threaded `std` library with an identified read-request/feed protocol, fragmented MFT bootstrap, bounded input chunks, record merging, tree linking and finalization.
- Produce an immutable, deterministic `Snapshot` and `ScanStats`, with separate unnamed logical size, ADS logical size and physical allocation accounting.
- Preserve all hard links, suppress DOS aliases, handle stale parents and cycles, and group NTFS metadata. Keep an `EntryRecord` boundary between NTFS phases 0–2 and generic phases 3–6.
- Extract shared development-only fixture support into `crates/adamite-fixtures`. Upgrade the independent manifests to schema v3 with allocation measurements and metafile entries; the user regenerates committed fixtures using sudo.
- Add snafu errors, workspace `tracing`, strict panic-free checks, fixture/property tests and full-scan fuzzing. Correct the architecture's core/I/O boundary and assert the node layout at compile time.

Out of scope: `adamite-io`/`VolumeReader` belongs to `add-volume-reader`; parallelism, buffer pools, `$BITMAP` skipping and general resource limits remain roadmap step 3. Query operations (`summarize`, `list_dir`, `search`) belong to `add-core-query`. Persistence, CLI, MCP, USN, walkdir scanning and automated WizTree/`dir /s` comparison are deferred; manual Windows comparison follows later.

## Scope areas

- `core/scan-protocol`: identified byte requests, response validation, progress and completion.
- `core/mft-bootstrap`: record 0, attribute lists, extension discovery and virtual-to-physical mapping.
- `core/record-merge`: classification, stream identities, accounting and diagnostic counts.
- `core/tree-linking`: parent validation, aliases, hard links, orphans, cycles and metadata grouping.
- `core/aggregation`: subtree totals, deterministic ordering, top files and extension statistics.
- `core/snapshot`: immutable arena, CSR children, names, cold metadata and minimal accessors.

## Impact

- New core and fixture crates inherit workspace edition, toolchain, license and lints. Existing NTFS parser requirements and `no_std` support remain intact; parser tests migrate to shared support and schema v3.
- Runtime dependencies: `adamite-ntfs`, snafu and workspace `tracing`. Development support uses `ruzstd`, serde, serde_json and proptest; the separate fuzz package uses `libfuzzer-sys`.
- `scripts/fixtures/**`, committed NTFS images/manifests, `scripts/check.ts`, `deno.json` and CI gain fixture and full-scan coverage. Normal tests require neither mounts nor system NTFS tools.
- Update docs/ARCHITECTURE.md. Later adapters serve byte requests, and later query consumers use the immutable snapshot.

## Design

1. D1. Crates and modules: core has `scan/{protocol,bootstrap,ingest,merge}.rs`, `entry.rs`, `tree/{link,aggregate}.rs`, `snapshot.rs` and `stats.rs`; `lib.rs` exposes the public types. `adamite-fixtures` is `publish = false`, used only through consumers' dev-dependencies, and depends on the parser but never on core. Put reusable synthetic builders there; keep parser-specific property configuration in parser tests. Core forbids unsafe code and denies Clippy `indexing_slicing`, `arithmetic_side_effects`, `unwrap_used`, `expect_used` and `panic`.

2. D2. Public driver contract: use the following signatures and documented public types. Non-exhaustive snafu enums distinguish ConfigError (invalid cap/chunk/size overflow), FeedError (unknown id/length/inactive/allocation), ScanError (geometry/bootstrap/overflow/allocation/inactive, with parser sources) and SnapshotError (path allocation failure). Core performs no file/device/network I/O, defines no reader trait or callback, creates no threads, reads no clocks and never sleeps; callers may drive it synchronously or asynchronously.

```rust
pub struct ScanConfig {
    pub max_outstanding: usize,
    pub chunk_size: usize,
    pub context: ScanContext,
}
pub struct RequestId(u64);
pub struct ReadRequest { pub id: RequestId, pub offset: u64, pub len: usize }
pub enum ScanStep { Need(ReadRequest), Progress(ScanProgress), Done(ScanOutput) }
pub struct ScanOutput { pub snapshot: Snapshot, pub stats: ScanStats }
impl Scanner {
    pub fn new(config: ScanConfig) -> Result<Self, ConfigError>;
    pub fn step(&mut self) -> Result<ScanStep, ScanError>;
    pub fn feed(&mut self, id: RequestId, bytes: &[u8]) -> Result<(), FeedError>;
}
```

3. D3. Defaults and waiting: `ScanConfig::default()` selects cap 1 and 1 MiB chunks; reject zero cap, chunk sizes outside 512 bytes–16 MiB, sizes not divisible by 512, and overflowing cap × chunk size. Each request is a nonempty volume-relative byte range no larger than `chunk_size`; the initial request is offset 0, length 512. `step` emits each monotonic checked request id once. `ScanProgress` contains phase, processed-record count, outstanding count and `waiting_for_input`; when no work is possible, return Progress with that flag set and the caller waits for its own I/O.

4. D4. Multiple requests: emit independent reads until the cap is reached; allow exact-length responses in any order. Keep request slots occupied until their responses are consumed in virtual order, so completed later responses cannot cause unlimited buffering. Retained transport bytes are bounded by cap × chunk size plus one record and attribute-list assembly storage; metadata arenas grow with observed records. `feed` copies retained bytes, keeps no caller borrow, and rejects unknown/completed ids, wrong lengths and inactive scans without changing state. Partial responses are rejected. `Done` transfers the output once; later calls return an inactive-state error.

5. D5. State transitions: `Boot -> Record0 -> Discover -> Stream -> Merge -> Link -> Aggregate -> Finalize -> Done`; unrecoverable failures enter `Failed`. Discover alternates list acquisition and reachable extension reads until mapping is complete. Stream assembles records from mapped physical pieces and parses them in increasing virtual-record order. CPU-only steps process at most 256 records/entries, or one directory sort, before returning Progress; no time-based yield policy is added. Enter a tracing span for work in each phase, without retaining an entered span across calls.

6. D6. Bootstrap: validate boot geometry, record 0's in-use base identity, unnamed non-resident DATA first segment, positive record-aligned MFT data size, run geometry and complete iterator termination. Track owned runs and pending `(segment FileRef, attribute type/name/id, lowest_vcn)` references. Read only extension records whose entire virtual byte range is mapped; check sequence, base reference and requested segment identity before committing new runs. Deduplicate identical discoveries, reject conflicting/overlapping mappings, and require gap-free coverage of the logical MFT data range before Stream. No mirror fallback is attempted.

7. D7. Attribute lists: support resident values and assemble non-resident values up to the parser's 256 KiB format bound using known descriptors. Parse a complete value with `AttributeList::new`; also inspect reachable extension records for additional list descriptors. If completing a list or reaching a listed segment requires unavailable mappings and a whole discovery pass adds nothing with no requests pending, return `BootstrapIncomplete`. Reject sparse/compressed MFT and bootstrap-list storage explicitly. Missing targets, stale references and malformed bootstrap values are fatal errors, never endless retries.

8. D8. Streaming and failure handling: split physical requests at extent and chunk boundaries; a record may span multiple responses, including `frag-512` record 4095. Use virtual ordinals as record identity and reject a mismatching stored NTFS 3.1 record number. Fix up each assembled raw record once and exhaust attributes and required runlists before publishing its contribution. Bootstrap rereads do not publish contributions or count records. Corrupt ordinary records are discarded and counted; deleted, BAAD and unused records never produce nodes. Invalid geometry/bootstrap, missing or corrupt root, arithmetic/index overflow and allocation failure are fatal. Grow slots incrementally and use fallible allocation before extending buffers.

9. D9. Intermediate storage: dense `Vec<RecordSlot>` is indexed by MFT ordinal, with classification, sequence, flags and optional owned base contribution. `Patch` carries base `FileRef`, source record reference, attribute id and owned contribution. Stream key is `(attribute type, raw UTF-16 name)`; segment key additionally includes lowest VCN and source identity. Reject conflicting first segments, overlapping/gapped non-resident segments and duplicate resident streams; discard affected ordinary base records, and reject stale/missing-base patches with a counter. Merge names, SI metadata and streams transactionally into `EntryRecord { file_ref, attrs, mtime, size, ads_size, alloc, names, nlink, reparse_tag, flags }`; generic tree code consumes only these entries.

10. D10. Accounting: `size` is unnamed DATA logical bytes; `ads_size` sums named DATA logical bytes, excluding `$BadClus:$Bad` logical bytes. Subtree logical totals exclude ADS. `alloc` sums all non-resident attribute streams, including DATA, ADS and INDEX_ALLOCATION, once per first segment; use `compressed_size` for compressed/sparse storage and `allocated_size` otherwise. Resident allocation is zero. Continuation sizes and FILE_NAME cached sizes never contribute. Reparse targets are not followed; their records retain their own sizes and allocation.

11. D11. Names and hard links: retain raw UTF-16 through alias selection and comparisons, then append lossy UTF-8 to the final pool; count invalid surrogate sequences. Deduplicate identical links and discard DOS namespace 2 names when that record has a non-DOS name under the same parent; preserve distinct non-DOS names, including same-parent hard links. If only DOS names exist, retain them. Choose the primary link by `(parent record number, raw UTF-16 name)` ascending, then namespace and parent sequence as tie-breaks. Every other link has HARDLINK_DUP, retains its self values, and contributes zero bytes and files to aggregation, top files and extension statistics.

12. D12. Tree validity: root record 5 is required, becomes NodeId 0 named `/`, and its self-parent name creates no edge. Valid parents must be in-use directories with matching sequences; otherwise attach the link to `<orphaned>`. For directories with multiple names, children attach to the primary directory node; secondary directory nodes have no duplicated children. Detect cycles iteratively, reparent every cycle-member directory to `<orphaned>` and retain its non-cycle descendants. Group in-use base records 0–15 except root, and direct `$Extend` children, under `<NTFS metadata>`; preserve descendants beneath those entries. Create synthetic directories only when needed, with zero self sizes and no FileRef; exclude them from file counts.

13. D13. Layout: use a flat arena and checked u32 node/CSR/name offsets. `#[repr(C)] Node` contains five u64 fields (`size`, `alloc`, `total_size`, `total_alloc`, `mtime`), five u32 fields (`parent`, `child_start`, `child_len`, `file_count`, `name_off`), then u16 `name_len` and `flags`, in that order. Assert `size_of::<Node>() == 64` at compile time; avoid a padded embedded NameRef. Keep `ads_size`, FileRef, raw attributes, nlink and reparse tag in parallel cold metadata. Reject representational overflow; nodes have no individual heap allocation. CSR stores each child once and names are slices of a UTF-8 byte pool.

14. D14. Snapshot access: all arrays are private and frozen on completion. Expose `node(NodeId) -> Option<NodeView<'_>>`, `children(NodeId) -> Option<&[NodeId]>`, `name(NodeId) -> Option<&str>` and `path(NodeId) -> Result<Option<String>, SnapshotError>`; NodeView has read-only scalar/cold-metadata getters. Snapshot exposes immutable volume, top_files and ext_stats slices; ScanOutput alone owns ScanStats. Assert Snapshot is Send + Sync and can be wrapped in Arc. Volume metadata contains parsed geometry/serial plus caller-supplied optional label, FILETIME scan timestamp and free bytes through ScanContext; absent context stays absent. No USN, measured durations or platform acquisition is added.

15. D15. Aggregation and determinism: assign synthetic ids in metadata/orphan order, then ordinary ids by `(record, parent record, raw name, namespace)`; root remains 0. Build CSR, compute BFS order and aggregate in reverse without recursion. Initialize self totals from size/alloc and file_count to one for files, zero for directories; suppress duplicate-link contributions. Sort children by `(-total_alloc, raw name, FileRef, NodeId)`; top_files is the first 100 primary files by `(-alloc, raw name, FileRef, NodeId)`, including metafiles. ext_stats groups primary files by the last ASCII-lowercased suffix without the dot, with empty extension for dotfiles/trailing-dot/no-dot names, and sorts by `(-total_alloc, extension)`. Names and output never depend on hash iteration or response order.

16. D16. ScanStats: use checked u64 counters `records_total`, `records_in_use`, `base_records`, `extension_records`, `unused`, `deleted`, `baad`, `corrupt_records`, `bad_fixup`, `invalid_fixup_metadata`, `invalid_signature`, `corrupt_headers`, `corrupt_attributes`, `type_storage_mismatches`, `corrupt_values`, `corrupt_runlists`, `out_of_volume_extents`, `invalid_attribute_lists`, `invalid_file_refs`, `invalid_utf16`, `rejected_extensions`, `corrupt_streams`, `orphans`, `cycle_nodes`, `hardlink_duplicates` and `metafiles`. Total counts each Phase-1 ordinal once; in_use is Base/Extension classification, even if later discarded. Error-category counts describe rejected contributions; orphans/cycles count reparented nodes and metafiles counts distinct grouped base records. Statistics exclude request/chunk counts and clocks, so chunk size does not change them.

17. D17. Manifest v3: retain v2 entry fields and add stream `attribute_type`, decimal-string `allocated_size` and nullable decimal-string `compressed_size`; streams include DATA and every other non-resident attribute type. Resident DATA has allocated_size `"0"` and compressed_size null; compressed/sparse non-resident streams require compressed_size. Add `metafiles: Entry[]` for in-use bases 0–15 except root plus `$Extend` children, and `fill` totals `total_size`, `total_ads_size`, `total_alloc` as decimal strings before compaction. Partition entries/metafiles without duplication; root stays in entries and metafile names/parents remain on-disk values. Readers accept v2/v3 during migration, default v2 attribute_type to DATA and missing measurements to absent, and require all v3 measurements; writers emit v3 only.

18. D18. Independent measurements: run `ntfsinfo -v -i <record> <image>` and parse each attribute's first-segment logical, allocated and compressed-size fields, grouping by type/name and summing run counts across segments. Never round logical sizes to estimate allocation or use Adamite to create expected values. Treat a missing required non-resident size as a generation error. Enumerate MFT ordinals through ntfsinfo to identify metadata bases and `$Extend` parent links, and require every live base to be covered by explicit entries, metafiles or filler summaries. Record tool versions, retain checksum/geometry checks and enforce the existing compressed-image budget. Parser assertions filter DATA streams where appropriate and test allocation fields through existing NonResident APIs.

19. D19. Fixture API: `Fixture::load(&Path) -> Result<Fixture, FixtureError>` verifies the zstd frame checksum, then delegates schema/length validation to `Fixture::from_image(Vec<u8>, Manifest) -> Result<Fixture, FixtureError>`. Expose `image(&self) -> &[u8]` and `manifest(&self) -> &Manifest`. Move the existing reference MFT bootstrap into separate `ReferenceMft::from_fixture(&Fixture) -> Result<ReferenceMft, FixtureError>` with geometry, extents, records and record accessors for parser tests. Core tests only use image/manifest, never ReferenceMft or manifest extents to seed Scanner. Share the existing boot/record/attribute/runlist builders without core dependencies.

20. D20. Verification: fixture drivers fill request batches directly from image ranges and feed them in varied orders. Compare paths, identities, flags, times, self sizes, ADS/allocation, metadata, filler totals, subtree totals, duplicate treatment, CSR order and derived indexes against manifests and an independent test-side tree calculation. Synthetic tests cover corruption, stale references, cycles, stream collisions, bootstrap stalls, split records and protocol misuse. Proptest varies chunk sizes, caps and feed permutations and requires equal snapshots/stats; merge unit tests check per-stream identities/run counts. Fuzz structured volumes and bounded mutations/protocol events: cap image size at 1 MiB, preflight advertised MFT size at 4096 records and lists at 256 KiB, use request caps 1–4 and chunks 512–4096 bytes, and assert termination within `4096 + 64 * records + 16 * image_sectors` driver calls. Production adds no general resource-budget API.

21. D21. Architecture edits: rewrite §1–§2 to show core emitting requests and caller-owned I/O; explain bootstrap ordering and cross-extent reassembly. Update §3 for stream identities, cold metadata, the 64-byte assertion, exact accounting and private immutable storage; separate minimal accessors from future §4 queries. Move §5 reading/alignment/buffer-pool ownership to future adapters and keep parallelism deferred. Update §6 implementation order and §7 tracing/error conventions to match this plan.

## Tasks

Each automated milestone is one bounded Codex run targeting 15–30 minutes, including its listed verification. Stop at the milestone boundary; split a milestone further before implementation if its concrete work exceeds that budget. Do not create commits without the user's separate review approval. Commands below run from the repository root unless a working directory is stated.

### M1 — Shared fixtures and workspace dependencies

- [x] M1.1 Add workspace tracing, create the unpublished fixture crate, extract the loader/reference bootstrap and reusable builders, and migrate parser tests without changing v2 behavior.
- [x] M1.2 Update lockfiles and dev-dependencies; retain parser allocation and embedded checks. Verify: `deno task check`.

### M2 — Schema v3 generator and compatible readers

- [x] M2.1 Implement D17–D18 in Deno collection/compaction and the shared manifest reader; add tool-output samples for resident, sparse, compressed, continuation and index streams, metadata enumeration and filler totals.
- [x] M2.2 Keep v2 tests green while adding strict v3 schema tests and parser allocation assertions. Verify: `deno test scripts/fixtures/`; `deno check scripts/`; `cargo test -p adamite-ntfs -p adamite-fixtures --locked`.

### M3 — USER STEP: regenerate and accept fixtures

- [x] M3.1 USER ACTION on Linux with ntfs-3g, zstd and attr: run `deno task fixtures` and complete its sudo prompts. This is the required external prerequisite for v3 end-to-end acceptance.
- [x] M3.2 After regeneration, inspect all three v3 manifests, metadata coverage, allocations, checksums and image budget. Verify: `cargo test -p adamite-ntfs -p adamite-fixtures --locked`; `deno test scripts/fixtures/`.

### M4 — Core skeleton and request protocol

- [ ] M4.1 Create modules, public types, config defaults, snafu errors, strict parser-equivalent Clippy denies and phase tracing. Implement identified request slots and transactional exact-length feeds, including cap > 1 and inactive-state behavior.
- [ ] M4.2 Add protocol tests for bounds, waiting, invalid ids, duplicate feeds and reordered responses. Verify: `cargo test -p adamite-core --locked protocol`; `cargo clippy -p adamite-core --all-targets -- -D warnings`.

### M5 — Geometry, mapping and record 0

- [ ] M5.1 Implement checked physical/virtual mapping, boot/record-0 requests, record assembly and owned first-segment runs/data size; test short geometry, invalid MFT, range overflow and no mirror fallback.
- [ ] M5.2 Verify split record-0 reads and runlist termination. Verify: `cargo test -p adamite-core --locked bootstrap_record0`.

### M6 — Attribute-list and extension bootstrap

- [ ] M6.1 Implement resident/non-resident list assembly, reachable extension discovery, segment-reference validation, duplicate detection, complete mapping validation and no-progress failure.
- [ ] M6.2 Add multi-pass discovery, stale/missing target, overlap/gap, unsupported storage and inaccessible-list tests. Verify: `cargo test -p adamite-core --locked bootstrap`.

### M7 — Bounded streaming ingestion

- [ ] M7.1 Stream mapped MFT ranges with capped requests and ordered record assembly; classify and transactionally decode each record into slots/patches without retaining the whole raw MFT.
- [ ] M7.2 Implement classification/parser counters and test cross-extent record 4095, partial records, deleted/BAAD/unused input and late parse errors. Verify: `cargo test -p adamite-core --locked ingest`.

### M8 — Extension merge and accounting

- [ ] M8.1 Implement stream/segment identities, FileRef patch checks, transactional merge, first-segment accounting, ADS/index/other non-resident allocation and BadClus exclusion; emit EntryRecord.
- [ ] M8.2 Test extensions preceding bases, missing/stale bases, duplicate first segments, overlap/gap, sparse/compressed sizes and discarded-base diagnostics. Verify: `cargo test -p adamite-core --locked merge`.

### M9 — Links, aliases and damaged parent graphs

- [ ] M9.1 Implement root handling, raw-name ordering, DOS suppression, all hard-link nodes and deterministic primary choice; add orphan/cycle repair and metadata grouping.
- [ ] M9.2 Test same-parent links, stale/non-directory parents, multi-name directories, self/cyclic references and synthetic groups. Verify: `cargo test -p adamite-core --locked linking`.

### M10 — Snapshot arena and CSR

- [ ] M10.1 Implement the exact Node layout/assertion, checked name arena and cold columns, deterministic ids and CSR construction; add immutable node/children/name/path accessors and Send + Sync checks.
- [ ] M10.2 Test root, invalid ids, Unicode, deep paths and representational bounds. Verify: `cargo test -p adamite-core --locked snapshot`; `cargo doc -p adamite-core --no-deps`.

### M11 — Aggregation and finalization

- [ ] M11.1 Add iterative subtree totals, duplicate contribution suppression, deterministic directory ordering, top-100 files and extension statistics; connect Finalize to one-shot Done.
- [ ] M11.2 Test ties, directory self-allocation, ADS exclusion from logical totals, extension edge cases and a deep nonrecursive tree. Verify: `cargo test -p adamite-core --locked aggregation`.

### M12 — Independent fixture end-to-end oracles

- [ ] M12.1 Drive Scanner from each decompressed image and compare all D20 outputs against v3 entries, metafiles and fill totals; independently compute expected subtree totals and final indexes.
- [ ] M12.2 Require fragmented bootstrap, record 4095 reassembly, same-parent hard links and 4Kn coverage. Verify: `cargo test -p adamite-core --test fixtures --locked`; `cargo test -p adamite-ntfs --locked`.

### M13 — Chunk and response-order properties

- [ ] M13.1 Add bounded synthetic-volume proptests varying chunk sizes, outstanding cap and response permutation; compare full snapshots and diagnostics and assert transport storage bounds.
- [ ] M13.2 Add shrinking-friendly regressions for bootstrap/merge ordering and no-progress termination. Verify: `cargo test -p adamite-core --test properties --locked` with `PROPTEST_CASES=1024`.

### M14 — Full-scan fuzz target

- [ ] M14.1 Create excluded `crates/adamite-core/fuzz`, target `scan`, structured bounded input generation, protocol mutations and a size-derived termination bound; seed from synthetic valid layouts and reduced fixture fragments.
- [ ] M14.2 Document the harness bounds and verify panic/request invariants. Verify in `crates/adamite-core`: `cargo +nightly fuzz run scan -- -max_total_time=30 -max_len=262144 -timeout=2 -rss_limit_mb=2048`.

### M15 — Gate and CI integration

- [ ] M15.1 Extend scripts/check.ts and deno permissions for both fuzz packages, core corpus generation/checks and Deno fixture tests; update script tests and Linux fuzz CI while retaining Windows workspace and parser no_std coverage.
- [ ] M15.2 Keep normal checks free of sudo/system NTFS tools. Verify: `deno test --allow-read --allow-write scripts/check_test.ts`; `deno task ci-lint`; `deno task check --fuzz`.

### M16 — Architecture documentation and final acceptance

- [ ] M16.1 Apply D21 to docs/ARCHITECTURE.md and document the request/feed example, defaults, error handling and shared fixture workflow in crate documentation; preserve the explicit future-work boundaries.
- [ ] M16.2 Review the implementation against this plan and record verification outcomes in repository documentation without local-only artifact links. Verify: `deno task check`; `deno test scripts/fixtures/`; confirm the Ubuntu/Windows CI results when available.
