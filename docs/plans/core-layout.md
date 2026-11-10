# Plan: adamite-core module layout

## Why

- `scan/` mixes the scanner, configuration and request protocol with NTFS bootstrap, discovery, mapping, ingestion and merge. `ScanError`, `ScanOutput` and `ScanPhase` sit inside the 611-line scanner state machine.
- Dependencies run back toward the driver: stages import `scanner::ScanError`, merge borrows bootstrap/ingest/mapping helpers, and finalization borrows aggregate helpers. Several modules and tests use `super::*`.
- Allocation, overflow, invalid-input, counter and index helpers are duplicated across bootstrap, mapping, merge, linking, snapshot building and aggregation.
- `tree/link` resolves the graph before snapshot construction; `tree/aggregate` and `tree/finalize` mutate snapshot storage afterward. This boundary exposes `SnapshotStorage`, `checked_u32` and `name_range` throughout the crate.
- Pipeline operations are free functions separate from the records, contributions, mappings and totals they construct or mutate.
- The root flattens more than 20 public names and houses both the mixed intermediate model in `entry.rs` and scan counters in `stats.rs`.
- The 2,881-line `scan/scanner/tests.rs` combines protocol and every processing phase.

## What Changes

Separate the scan driver from private NTFS processing, move generic entries/linking into `graph/`, and put snapshot construction, aggregation and finalization under private `snapshot::build`. Give pipeline operations type owners, consolidate fallible collection helpers, and expose each public item through one domain path.

Dependency: begin implementation only after [ntfs-layout](ntfs-layout.md) M1–M5 are complete and its final checks pass. Core will already call `adamite_ntfs::record::FileRecord::parse`, `adamite_ntfs::boot::VolumeGeometry::from_boot_sector` and `adamite_ntfs::record::RecordClass::classify`, with the new parser module imports. Use that completed state as the regression baseline.

## Scope

Module/API paths, internal operation ownership, visibility, test organization and associated documentation. Preserve public type semantics, signatures apart from import paths, derives, error variants/fields/Display/source chains, allocation policy, counter accounting and deterministic output. Preserve the state transitions and work bounds from [core-scan D5](core-scan.md): at most 256 per-item operations per call, immediate yield after one directory sort, and the existing bulk-memory and attribute-list exceptions. Keep phase tracing, transport bounds and transactional feed/record/patch behavior unchanged.

Follow the project's Rust best practices. Production types must not carry fields, variants or hook state used only by tests. Add no dependencies, features or bash/Python project scripts. Retain fixtures, fuzz input formats, target names, preflight/termination bounds and corpus filenames/bytes.

## Impact

Migrate core source/unit tests, `tests/{fixtures,properties,protocol}.rs`, the excluded core fuzz package (`fuzz_targets/scan/{driver,input}.rs` and `tests/harness.rs`), the crate's runnable driver doctest and README imports. Update the core module descriptions and diagram in `docs/ARCHITECTURE.md`. Existing fixture/property/protocol assertions and the fuzz harness remain the regression oracle. This change owns core and those documentation edits; `scripts/check.ts`, other scripts, `deno.json`, fixtures and `crates/adamite-ntfs` remain outside its edit scope.

## Design decisions

1. D1. Layout and public paths: files below are relative to `crates/adamite-core/src/`. `lib.rs` retains crate docs and existing safety/lint attributes, declares public `error`, `scan`, `snapshot` and private `graph`, `mft`, `util`, and contains no re-exports or type checks. Explicit facade re-exports from private implementation children are allowed inside scan/snapshot. All scan child modules are private; `scan/mod.rs` explicitly exports the full scan API below, giving each item exactly one public path under `scan::`.

Delete both `const _: fn() = || { fn assert_send... }` blocks from `src/lib.rs` and `src/snapshot/storage.rs`. Put all thread-safety verification in `crates/adamite-core/tests/thread_safety.rs`, importing core types only through public D1 paths. Use plain generic functions `assert_send<T: Send>()` and `assert_send_sync<T: Send + Sync>()` to check `scan::{Scanner, ScanOutput, ScanStats}` for Send and `snapshot::Snapshot` for Send + Sync. Add a real cross-thread test: obtain a single-threaded fixture scan result, move a Scanner and the fixture image into `std::thread::spawn`, complete the scan there, and return its ScanOutput to the main thread through `join`. Compare the complete output with the single-threaded result, then share its snapshot through `Arc<Snapshot>` across several threads reading nodes, children, names, paths and indexes concurrently; compare their observations with the single-threaded snapshot. This keeps the checks in one consumer-facing test without closure/const assertions in production; the gate and CI always compile tests.

| File / module                                                                             | Items and visibility                                                                                                                                                                              |
| ----------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `error.rs` (public)                                                                       | `error::ScanError`, its conversions and crate-private constructors                                                                                                                                |
| `scan/mod.rs` (public)                                                                    | `scan::{Scanner, ScanStep, ScanOutput, ScanPhase, ScanProgress, ScanConfig, ScanContext, ConfigError, ReadRequest, RequestId, FeedError, ScanStats}`; explicit exports from private child modules |
| `scan/scanner.rs`, `scan/status.rs` (private)                                             | Driver implementation; separate public step/output/phase/progress definitions                                                                                                                     |
| `scan/config.rs` (private)                                                                | Defines `ScanConfig`, `ScanContext`, `ConfigError`; exported only as `scan::{ScanConfig, ScanContext, ConfigError}`                                                                               |
| `scan/protocol.rs` (private)                                                              | Defines `ReadRequest`, `RequestId`, `FeedError`; exported only as `scan::{ReadRequest, RequestId, FeedError}`; private slots and scan-only `Requests`                                             |
| `scan/stats.rs` (private)                                                                 | Defines `ScanStats`, exported only as `scan::ScanStats`; moved from root with the counter documentation supplied by ntfs-layout M5                                                                |
| `mft/mod.rs` (private)                                                                    | Declarations and explicit crate-private stage exports                                                                                                                                             |
| `mft/{bootstrap,discovery,mapping,ingest,merge}.rs`, `mft/ingest/decode.rs` (private)     | Existing NTFS stages and their type-owned operations                                                                                                                                              |
| `mft/model.rs` (private)                                                                  | `RecordKind`, `RecordSlot`, `RecordContribution`, `Patch`, `NameAttribute`, `StandardAttribute`, `ReparseAttribute`, `StreamSegment`, `StreamStorage`, extracted from `entry.rs`                  |
| `graph/mod.rs`, `graph/{entry,link}.rs` (private)                                         | `EntryRecord`, `EntryName`, `Linker` and link-local working state; use snapshot `NodeFlags` directly instead of the `EntryFlags` alias                                                            |
| `snapshot/mod.rs` (public)                                                                | Canonical `snapshot::{Snapshot, Node, NodeId, NodeView, NodeFlags, VolumeInfo, ExtensionStat, SnapshotError}` via explicit private-child exports                                                  |
| `snapshot/{node,storage,indexes,error}.rs` (private)                                      | Existing read types/storage/indexes; separate leaf definition of `SnapshotError`                                                                                                                  |
| `snapshot/build/mod.rs`, `snapshot/build/{input,builder,aggregate,finalize}.rs` (private) | Construction input `ParentTree`, `ResolvedNode`, `NameRange`; `SnapshotBuilder`, `Aggregator`, `Finalizer`; limited crate-private facade exports for graph/driver access                          |
| `util.rs` (private)                                                                       | Fallible collection and counter extension traits; no domain errors                                                                                                                                |

For example, consumers import `adamite_core::scan::Scanner`, `adamite_core::scan::ScanConfig`, `adamite_core::scan::ScanStats`, `adamite_core::snapshot::Snapshot` and `adamite_core::error::ScanError`. Remove root `entry.rs`, `stats.rs`, `tree/` and the five NTFS stage files under `scan/` after migration.

2. D2. One-way dependencies: `A -> B` means A directly uses B. The processing order is `util <- snapshot types/build input <- graph <- mft <- scan driver`; the driver also orchestrates private snapshot build passes. The flat public scan facade does not change internal dependencies: private `scan::stats` and `scan::config` remain independent leaves. `ScanContext` stays defined in config and is exported as `scan::ScanContext`, preserving the existing `VolumeInfo::context() -> &ScanContext` signature. The table uses internal module paths. `ScanError` is a stage-independent error leaf; its core dependencies are the leaf `SnapshotError` that it wraps and domain-free `util::CounterOverflow`.

| Source module                                            | Allowed direct dependencies within core                                                                              |
| -------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------- |
| `util`, `scan::stats`, `scan::config`, `snapshot::error` | None                                                                                                                 |
| `error`                                                  | `snapshot::SnapshotError`, `util::CounterOverflow`                                                                   |
| `snapshot::{node,indexes}`                               | `snapshot::SnapshotError` for checked index conversion                                                               |
| `snapshot::storage`                                      | Snapshot node/index/error types, `scan::config::ScanContext`, `util`                                                 |
| `snapshot::build::input`                                 | Snapshot node/error types, `util`                                                                                    |
| `snapshot::build::builder`                               | Snapshot types/storage/error, build input, `scan::config::ScanContext`, `util`                                       |
| `snapshot::build::{aggregate,finalize}`                  | Snapshot types/storage/error, `util`; neither imports the other                                                      |
| `graph::entry`                                           | Snapshot node/build-input types, `error`, `util`                                                                     |
| `graph::link`                                            | `graph::entry`, snapshot node/build-input/error types, `scan::stats`, `error`, `util`                                |
| `mft::model`                                             | None                                                                                                                 |
| `mft::mapping`                                           | `error`, `util`                                                                                                      |
| `mft::bootstrap`                                         | `mft::mapping`, `error`, `util`                                                                                      |
| `mft::discovery`                                         | `mft::{bootstrap,mapping}`, `error`, `util`                                                                          |
| `mft::ingest::decode`                                    | `mft::model`, `scan::stats`, `error`, `util`                                                                         |
| `mft::ingest`                                            | `mft::{model,mapping,ingest::decode}`, `scan::stats`, `error`, `util`                                                |
| `mft::merge`                                             | `mft::model`, `graph::entry`, snapshot `NodeFlags`, `scan::stats`, `error`, `util`                                   |
| `scan::protocol`                                         | `error`, `util`                                                                                                      |
| `scan::status`                                           | `scan::{protocol,stats}`, snapshot public types                                                                      |
| `scan::scanner`                                          | `scan::{status,config,protocol,stats}`, MFT stages, graph entry/linker, snapshot types/build facade, `error`, `util` |

Facade declarations/exports and documentation links are excluded; external parser types use ntfs-layout's canonical paths. Resolve relative imports and inspect moved impl blocks as well as imports. Test modules may depend on other stages for fixtures and orchestration; production edges must match this table. Never import scanner/status/protocol/config into MFT code. Keep statistics as passive counters: diagnostic policy belongs to the stage processing the error, avoiding a reverse dependency from stats to MFT errors.

3. D3. Snapshot owns its build boundary. Move `ParentTree` and `ResolvedNode` declarations from linking to `snapshot/build/input.rs`; graph fills these lower-layer input values, and the builder consumes them without importing graph. Put checked name offset/length construction on `NameRange::new`. Keep required checked conversions on `NodeId` or at the conversion site, preserving existing error field strings. Remove the shared free `checked_u32`/`name_range` exports. Keep graph-specific constructors for resolved input values in graph impl blocks.

Move aggregate/finalize and their tests into `snapshot/build/`. Restrict `SnapshotStorage`, its fields, `Snapshot::{from_storage,data,data_mut}`, node internals and extension-summary fields to snapshot-family visibility (`pub(super)` or `pub(in crate::snapshot)` as needed). Only build-input values and the three bounded pass types cross the snapshot facade with crate-private visibility; storage never does. Replace aggregate's shared `has`/`primary_file` with snapshot-private `Node::{has,is_primary_file}` methods. Keep builder, aggregate and finalize as separate incremental passes so the public phases and yield points remain intact.

4. D4. Errors and utilities. Keep `ConfigError` beside validated configuration, `FeedError` beside request ids/feeds, and `SnapshotError` inside snapshot; moving all errors into one enum would couple unrelated domains. Move the existing `ScanError` definition and `From<SnapshotError>` to `error.rs`, preserving the post-NTFS baseline's parser sources and error precedence, including separate `Geometry`, `Record` and `Fixup` variants.

Replace free error helpers with crate-private constructors `ScanError::{overflow,bootstrap,bootstrap_record,stream,link,link_overflow}` and `SnapshotError::{overflow,invalid}`. Pass the existing operation/reason/field literals, including link's `"tree linking"` overflow context. Map allocation failures to the caller's existing scan/feed/snapshot variant; preserve reserve-before-mutation ordering and source chains.

Consolidate `push`, `reserve`, `copy_slice`, `bump` and `increment` into `util` extension traits: `VecExt::{try_push,try_reserve,try_reserve_exact}`, `SliceExt::try_copy` and `CounterExt::try_bump`. Use method calls where unambiguous and qualified trait calls where Vec already has an inherent reservation method. Helpers return `TryReserveError` or `CounterOverflow` without importing domain errors. `ScanError` and `SnapshotError` convert `TryReserveError` into their sole allocation variant; feed errors retain explicit allocation mapping. `ScanError::from(CounterOverflow)` supplies the existing `"scan counter"` context, while link counters explicitly retain `"tree linking"`. Preserve exact versus amortized reservations; keep link-storage u32 prechecks in private `Linker::push_bounded` and merged-contribution length prechecks in private `Merge::reserve_contribution`, both delegating collection growth to `VecExt`. Checked integer conversions use standard `try_from` plus the domain constructor rather than duplicate `index` wrappers.

5. D5. Type-owned operations. Move each operation's body and its tests with its owner; do not add namespace-only wrapper structs. Keep NTFS contribution-to-entry conversion in MFT so graph never imports `RecordContribution` or `Patch`.

| Existing operation                                                   | Owner after restructuring                                                                                                                                  |
| -------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `bootstrap::{parse_record0,parse_extension,parts}`                   | `Record0::parse`, `RecordParts::{parse_extension,from_record}`                                                                                             |
| `bootstrap::{collect_runs,collect_mapping,check_storage}`            | `MftMapping::{from_segment,from_runlist}` and its private construction validation                                                                          |
| `bootstrap::list_descriptor`, `discovery::list_lowest`               | `AttributeListDescriptor::{from_attribute,lowest_vcn}`                                                                                                     |
| `decode::contribution`, `ingest::set_header`                         | `RecordContribution::decode`, `RecordSlot::set_header`                                                                                                     |
| `ingest::record_error`, `decode::{contribution_error,value_counter}` | `Ingest::record_error`, `DecodeError::account`; inline value categorization in the latter                                                                  |
| `merge::{apply_patch,entry,stream,lowest,entry_flags}`               | `Merge::apply_patch`, `RecordContribution::into_entry`, `StreamTotals::from_segments`, `StreamSegment::lowest_vcn`, private contribution flag construction |
| `link::{select_names,unnamed,node,synthetic,append_name}`            | `EntryRecord::{select_names,fallback_name,resolved_node}`, `ResolvedNode::synthetic`, `Linker::append_name`                                                |

Retain only these module-level production helpers: `decode::name` adapts the borrowed UTF-16 iterator into fallibly reserved owned units, `merge::compare_segments` is a local sort comparator, and `finalize::extension` is a local borrowed-string suffix selector. They contain no pipeline orchestration or cross-stage policy and are never imported by siblings. Test-only builders, drivers and assertions may remain free functions. Existing meaningful methods on discovery, mapping, assembly, linker and snapshot passes remain methods.

6. D6. MFT has no request protocol. Move bootstrap/discovery/mapping/ingest/merge to `mft/`, with the NTFS-only part of `entry.rs` in `mft/model.rs`. Discovery exposes bounded CPU progress, the next `ReadPiece`, successful-read planning and byte acceptance through methods; ingest exposes next-piece planning and bounded prefix consumption. Their results contain offsets, lengths, consumed-byte counts and progress/completion, never request ids, `ReadRequest`, `Requests` or `ScanConfig`.

The scan driver issues the returned range through `Requests`, commits planning only after successful issue, and supplies bytes in virtual order. Keep a partial response in its slot until its prefix is fully consumed. Move combined queue-plus-record capacity checks and waiting/cap decisions into the driver; preserve their existing errors and limits. Share one remaining 256-item budget across all prefix-consumption work in a driver call. Preserve issue-before-consume ordering, discovery cursors, no-progress detection, phase changes and trace-span lifetime; extract the existing sequence rather than introducing another public driver protocol or reader trait.

7. D7. Explicit imports and tests. Forbid `use super::*` in production and tests, including phase support files: explicit imports make stage dependencies visible during later moves. Group each module's unit tests under one file-level `#[cfg(test)] mod tests`, inline or in its existing tests file. Split scanner tests into `scan/scanner/tests/{mod,support,protocol,bootstrap,discovery,stream,merge,link,snapshot,done}.rs`; `mod.rs` declares phase modules, and support contains shared synthetic builders/drivers with explicit imports and test-only visibility. The done group includes full aggregation/finalization and one-shot completion cases. Keep merge, mapping, discovery, graph linking and snapshot-pass unit tests beside their owners. Preserve test names/assertions and observe existing cursors, collections and outputs for batching checks.

## Tasks

Each milestone is one bounded agent run of at most 30 minutes, including verification. Run sequentially from the completed NTFS baseline; split any milestone further before starting if its concrete work cannot fit. End every run with `deno task check` green and present that atomic change's diff and targeted validation for human review. Create or rewrite commits only after explicit approval; use Conventional Commits. Temporary root exports may retain the old core paths until M6, without adding new public aliases.

### M1 — Error leaves, counters and shared helpers

- [x] Record the completed-NTFS baseline diff and both corpus filename/byte hashes. Extract `error.rs` and `snapshot/error.rs`, move stats to private `scan/stats.rs` while retaining its existing root export temporarily, and establish the independent private config/stats leaves. Keep config/protocol/stats modules private throughout migration; M6 publishes their flat facade exports. Retarget existing error imports and add D4 associated constructors.
- [x] Add the util extension traits; replace duplicate allocation/counter/error helpers while preserving each caller's checks, reservation policy and error strings. Verify existing error/accounting tests; finish with `deno task check` green.

### M2 — Snapshot build ownership and visibility

- [x] Move build input declarations and name-range validation into `snapshot/build/input.rs`; retarget linker and builder. Move builder/aggregate/finalize and their unit tests under private build, replacing aggregate helper borrowing with Node methods.
- [x] Apply D3 visibility and remove crate-wide storage/checked-helper exports. Verify snapshot, aggregation and directory-sort batching regressions; finish with `deno task check` green.

### M3 — Graph and intermediate model separation

- [x] Move `EntryRecord`/`EntryName` and linking to graph; put NTFS slot/contribution/patch/stream models in private `mft/model.rs`. Retarget current stages and unit tests, remove `EntryFlags`, and give link operations their D5 owners.
- [x] Remove the remaining `tree/` and root `entry.rs` declarations/files. Verify linking, aliases, cycles, raw-name ordering and fixture paths; finish with `deno task check` green.

### M4 — Bootstrap and discovery without protocol dependencies

- [x] Move mapping/bootstrap/discovery into MFT, apply their D5 constructors and remove borrowed sibling helpers. Adapt the driver's record-zero/discovery handoff and read planning per D6.
- [x] Preserve request ordering, the 300-reference yield regression, list bounds and finite bootstrap failure. Keep ingest/merge temporarily under scan for M5. Verify bootstrap and protocol cases; finish with `deno task check` green.

### M5 — Ingestion and merge ownership

- [ ] Move ingest/decode/merge into MFT, apply the remaining D5 owners, and move request-queue/capacity/waiting work into the driver. Preserve prefix consumption and one per-call work budget; remove all five old scan-stage modules and protocol imports from MFT.
- [ ] Verify record 4095 reassembly, diagnostic counters, late-decode rejection, transactional patches, stream accounting and 256-item batches. Finish with `deno task check` green.

### M6 — Canonical public paths and driver contract types

- [ ] Extract status types from scanner; expose the D1 modules/facades, with `scan/mod.rs` explicitly exporting the complete flat D1 scan API and all scan child modules remaining private. Remove every root re-export and temporary alias, and delete both production Send-check blocks specified in D1.
- [ ] Add `tests/thread_safety.rs` with D1's plain generic trait checks, a complete fixture scan on a spawned thread with Scanner moved into it and ScanOutput returned, and concurrent `Arc<Snapshot>` reads compared with the single-threaded result. Use only public D1 paths. Verify `cargo test -p adamite-core --test thread_safety --locked`.
- [ ] Migrate integration tests, excluded fuzz consumers, README/rustdoc imports and links in the same run. Retain the runnable driver doctest and counter documentation. Verify doctests and core fuzz harness tests; finish with `deno task check` green.

### M7 — Phase tests, architecture and final verification

- [ ] Split scanner tests per D7, remove remaining `super::*` imports, update the core architecture diagram/module description and README, and audit D1–D6 against final code.
- [ ] Compare fuzz corpus path/byte hashes with the completed-NTFS baseline before and after smoke testing, including untracked seeds. Use the existing gate's scratch output; preserve any pre-existing unrelated workspace changes. Finish with `deno task check` and `deno task check --fuzz` green and record commands/results in the review.

## Acceptance criteria

- [ ] `rg -n 'assert_send' crates/adamite-core/src` has no matches. `crates/adamite-core/tests/thread_safety.rs` exists and `cargo test -p adamite-core --test thread_safety --locked` passes, covering the public trait bounds, complete cross-thread fixture scan/output transfer and concurrent shared-snapshot reads specified in D1.
- [ ] `rg -n '^pub use\b' crates/adamite-core/src/lib.rs` has no matches. Inspect `rg -n '^pub mod\b|^mod\b' crates/adamite-core/src/lib.rs` and generated rustdoc against D1; root contains only docs, existing attributes and module declarations. `rg -n '^\s*pub(\([^)]*\))?\s+mod\b' crates/adamite-core/src/scan/mod.rs` has no matches. Inspect `rg -n -U '^pub use[^;]*;' crates/adamite-core/src/scan/mod.rs` against the complete D1 scan API; each item has exactly one public path under `scan::`.
- [ ] `rg -n '^mod (entry|tree|stats);$' crates/adamite-core/src/lib.rs`, `rg -n 'crate::(entry|tree)::|super::scanner::ScanError|scan::(bootstrap|discovery|mapping|ingest|merge)::' crates/adamite-core/src crates/adamite-core/tests crates/adamite-core/fuzz --glob '*.rs'` and `rg -n 'use\s+super::\*' crates/adamite-core --glob '*.rs'` have no matches. Old root `entry.rs`/`stats.rs`, `tree/` and the monolithic scanner test file are absent.
- [ ] `rg -n '\b(pub\(crate\)\s+)?(struct SnapshotStorage|fn (data|data_mut|from_storage|checked_u32|name_range))\b|pub\(crate\).*SnapshotStorage' crates/adamite-core/src` is reviewed: storage/access methods have snapshot-family visibility, old checked helpers are absent, and no facade exports storage. `rg -n 'crate::(graph|mft)|scan::(scanner|protocol|status)' crates/adamite-core/src/snapshot` has no production matches.
- [ ] Enumerate `rg -n -U 'use\s+(crate|super|self)::[^;]*;' crates/adamite-core/src` and fully qualified paths/moved impl bodies; resolve every production dependency against D2 and confirm a topological ordering. `rg -n '\b(Requests|ReadRequest|RequestId|ScanConfig|Scanner)\b|crate::scan::(protocol|config|scanner|status)' crates/adamite-core/src/mft` has no production matches. Tests may reference driver types as specified in D7.
- [ ] Review `rg -n '^\s*(pub(\([^)]*\))?\s+)?fn\s+(parse_record0|parse_extension|parts|collect_runs|collect_mapping|check_storage|list_descriptor|list_lowest|contribution|contribution_error|value_counter|record_error|apply_patch|entry|stream|lowest|entry_flags|select_names|unnamed|node|synthetic|append_name|overflow|invalid|unsupported|push|reserve|bump|increment|index)\b' crates/adamite-core/src`: production pipeline/error/collection operations belong to D4–D5 impls/traits; the only remaining module-level production helpers are the three listed in D5. Test helper matches are reviewed separately.
- [ ] Inspect `rg -n -U 'adamite_core::[^;]*;|crate::(Scan[A-Za-z]*|Scanner|Snapshot[A-Za-z]*|Node[A-Za-z]*|ConfigError|FeedError|ReadRequest|RequestId|VolumeInfo|ExtensionStat)\b' crates/adamite-core docs/ARCHITECTURE.md --glob '*.rs' --glob '*.md' --glob '!corpus/**'`: active imports/examples use D1 paths, with scan items directly under `scan::` and no old root item access. `rg -n 'adamite_core::scan::(config|protocol|stats)\b' crates/adamite-core docs/ARCHITECTURE.md --glob '*.rs' --glob '*.md' --glob '!corpus/**'` has no matches; inspect grouped imports in the broader search too. Historical plans remain unchanged.
- [ ] Existing fixture/property/protocol oracles, core fuzz harness tests, the runnable driver doctest, error/source regressions and per-pass batching tests pass. `deno task check` supplies the 1,024-case property run, formatting, Clippy, documentation, workspace/fuzz tests and parser embedded checks; `deno task check --fuzz` also passes. Corpus path/hash comparisons show no additions, removals or byte changes; fixtures, fuzz formats/limits/targets, scripts and parser source remain unchanged by this work.
