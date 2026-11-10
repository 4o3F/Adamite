# Plan: adamite-ntfs module layout

## Why

- The crate root re-exports roughly 45 mixed parser names and system constants, while only `fixup` is a public module; import paths are inconsistent.
- `record` and `fixup` share rules in both directions, and `record` and `attribute` depend on each other through attribute iteration. Non-resident headers use a top-level runlist module that belongs with attributes.
- The 409-line `attribute/mod.rs` mixes type codes, flags, header parsing, iteration, typed-value helpers and two error enums. Header descriptors and typed decoders share a directory, with interleaved module declarations and re-exports.
- `file_ref.rs` combines `FileRef` with 13 system record-number constants.
- Parser error rustdoc and the README describe downstream `ScanStats` counter policy.
- Tiny private `bytes.rs` and `iter.rs` modules occupy the crate root.
- Public free functions separate operations from their types and require callers to compose fixup with record parsing.

## What Changes

Move declarations and implementations into public modules with one public path per item. Rename `geometry` to `boot`, `GeometryError` to `BootError`, `nonresident` to `non_resident`, and system constants as specified below. Merge private helpers into `util.rs`. Put boot decoding on `VolumeGeometry`, classification on `RecordClass`, and header validation plus fixup inside `FileRecord::parse`; update all consumers.

## Scope

Module/API organization, test organization and associated documentation. The intentional behavior change is that direct record parsing validates headers before fixup and leaves the buffer unchanged on every error. Core bootstrap and fixtures currently apply fixup first, so malformed records may now select a header error before a fixup error; D3 specifies downstream mapping. All other behavior stays unchanged, including classification precedence, individual validation rules, error variants/fields and Display text, allocation behavior, `Attribute` accessors and typed-value methods. Preserve `no_std`, lints, dependencies, fixtures and fuzz input formats.

## Impact

Update `adamite-core` source and unit tests, `adamite-fixtures/src/fixture.rs`, parser integration tests, both excluded fuzz packages, and README/rustdoc examples. Core integration tests currently have no direct parser imports; retain their fixture/property coverage. Update the parser description in `docs/ARCHITECTURE.md`. Scripts reference fuzz target names and the selector source rather than Rust imports; retain those paths, including `typed_values/selectors.rs`.

## Design decisions

1. D1. Public layout: paths below are relative to `crates/adamite-ntfs/src/`; every listed module is public except `util`. `lib.rs` contains crate documentation, existing attributes/lints, `pub mod` declarations and private `mod util` only. Use no public re-exports, including in parent modules. All public operations are associated functions or methods.

| File / module                   | Public items                                                                                                                                     |
| ------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------ |
| `boot.rs`                       | `VolumeGeometry`, `BootError`                                                                                                                    |
| `record/mod.rs`                 | `FileRecord`, `RecordHeader`, `RecordFlags`, `RecordClass`, `RecordError`                                                                        |
| `record/fixup.rs`               | `FixupError` only                                                                                                                                |
| `record/reference.rs`           | `FileRef`, `FileRefError`                                                                                                                        |
| `record/system.rs`              | `MFT`, `MFT_MIRROR`, `LOG_FILE`, `VOLUME`, `ATTR_DEF`, `ROOT`, `BITMAP`, `BOOT`, `BAD_CLUS`, `SECURE`, `UP_CASE`, `EXTEND`, `FIRST_NON_RESERVED` |
| `attribute/mod.rs`              | `Attribute`, `AttributeType`, `AttributeFlags`, `AttributeError`                                                                                 |
| `attribute/iter.rs`             | `AttributeIter`                                                                                                                                  |
| `attribute/non_resident.rs`     | `NonResident`, `StreamSizes`                                                                                                                     |
| `attribute/runlist.rs`          | `Runlist`, `Run`, `VcnRange`, `RunlistError`                                                                                                     |
| `value/mod.rs`                  | `ValueError`                                                                                                                                     |
| `value/standard_information.rs` | `StandardInformation`, `FileAttributes`, `FileTimes`                                                                                             |
| `value/file_name.rs`            | `FileName`                                                                                                                                       |
| `value/attribute_list.rs`       | `AttributeList`, `AttributeListEntry`, `AttributeListError`                                                                                      |
| `value/data.rs`                 | `Data`                                                                                                                                           |
| `value/index_allocation.rs`     | `IndexAllocation`                                                                                                                                |
| `value/reparse_point.rs`        | `ReparsePoint`                                                                                                                                   |
| `value/bitmap.rs`               | `Bitmap`, `BitmapIter`, `BitmapError`                                                                                                            |
| `utf16.rs`                      | `Utf16Le`, `StrictChars`, `LossyChars`, `Units`, `Utf16Error`                                                                                    |
| `util.rs` (private)             | None; existing byte and iterator helpers                                                                                                         |

All 13 constants retain their `u64` types and values; remove exactly the `_RECORD` suffix. For example, callers use `record::system::ROOT` and `value::file_name::FileName`.

The public-function audit found exactly three free functions: `parse_boot_sector`, `apply` and `classify`. Replace them with `VolumeGeometry::from_boot_sector(input: &[u8]) -> Result<VolumeGeometry, BootError>`, `FileRecord::parse(input: &mut [u8]) -> Result<FileRecord<'_>, RecordError>`, and `RecordClass::classify(input: &mut [u8]) -> RecordClass<'_>`. Other operations, including `RecordHeader::peek`, typed decoders, run validation and iterator constructors, already belong to types and retain their interfaces.

2. D2. Record parsing owns the pipeline: validate size, peek the header, validate FILE signature, allocated/used byte counts, first-attribute offset and USA offset in today's classification order, then apply fixup and construct the borrowed view. Preserve fixup's internal validation order and two-pass mutation: all fallible checks precede writes, with no fallible step after restoration. Wrap fixup errors as `RecordError::Fixup { source }`. Direct parsing accepts valid FILE headers regardless of IN_USE.

Use private associated helpers for header validation and construction from a validated header. `FileRecord::parse` calls both; `RecordClass::classify` shares them, preserving size -> uniform fill -> BAAD -> header validation -> IN_USE -> fixup -> base/extension precedence. Deleted/unused/BAAD/corrupt classifications leave bytes unchanged; classification applies fixup only to valid in-use records. Keep header/fixup logic shared rather than duplicating it.

`record::fixup` remains public solely to expose `FixupError` at one path. Its `apply` function, `FixedRecord` token and token accessor become `pub(super)`, accessible only within record; the token's fields stay private. The shared `valid_record_size` and `valid_usa_offset` rules also live there with `pub(super)` visibility. The record module uses these internals; fixup depends only on util.

Keep `FileRecord::attributes()`; move its existing checked region calculation into that method and pass `Option<&[u8]>` to `attribute::iter::AttributeIter::new`, preserving lazy `InvalidRegion` errors. The iterator no longer imports `FileRecord`. Its public `#[cfg(fuzzing)] from_region` remains in `attribute/iter.rs` and delegates to the same initialization with `Some(region)`.

Move `NonResident::parse` and `read_size` beside `Attribute::parse` in `attribute/mod.rs`, retaining their statements and order. Keep descriptor types and read accessors in `non_resident.rs`; make only fields needed by the parent parser `pub(super)`. This removes the descriptor module's reverse dependency on attribute flags/errors.

The exact allowed direct source dependencies are below; `A -> B` means A uses B. Module declarations and documentation links are excluded. `boot` and `utf16` independently use `util`; boot needs no UTF-16 dependency.

| Source module                                         | Allowed dependencies within this crate                                                    |
| ----------------------------------------------------- | ----------------------------------------------------------------------------------------- |
| `util`, `record::reference`, `record::system`         | None                                                                                      |
| `boot`, `utf16`                                       | `util`                                                                                    |
| `record::fixup` (internal repair, public error only)  | `util`                                                                                    |
| `attribute::runlist`                                  | `boot`, `util`                                                                            |
| `attribute::non_resident`                             | `attribute::runlist`                                                                      |
| `attribute`                                           | `attribute::non_resident`, `attribute::runlist`, `utf16`, `util`                          |
| `attribute::iter`                                     | `attribute`, `util`                                                                       |
| `record`                                              | `record::fixup`, `record::reference`, `attribute::iter`, `util`                           |
| `value`                                               | `attribute`                                                                               |
| `value::standard_information`, `value::reparse_point` | `value`, `attribute`, `util`                                                              |
| `value::file_name`                                    | `value`, `attribute`, `value::standard_information`, `record::reference`, `utf16`, `util` |
| `value::attribute_list`                               | `value`, `attribute`, `record::reference`, `utf16`, `util`                                |
| `value::data`, `value::index_allocation`              | `value`, `attribute`, `attribute::non_resident`, `utf16`                                  |
| `value::bitmap`                                       | `util`                                                                                    |

3. D3. Migrate core `scan/bootstrap.rs` record-zero and MFT-extension parsing, and both parsing sites in fixtures `fixture.rs`, from apply-then-parse to `FileRecord::parse(&mut bytes)`. Keep separate `ScanError::Fixup` and `FixtureError::Fixup` variants: map `RecordError::Fixup { source }` to them, and other `RecordError` values to the respective `Record` variant. Header errors now win over fixup errors, including shared size/USA rule failures. Preserve nested sources and existing variant/Display definitions; streaming classification and its `ScanStats` mapping remain unchanged.

4. D4. Keep helpers private unless another module needs access. Parent-private `value::ensure_type` and `resident_value` are accessible to decoder descendants; `FileTimes::parse` remains `pub(super)`. Use `pub(crate)` for shared util functions and the iterator constructor called by record. Keep error selectors private to their module family and grant only required visibility. Add no test-only fields, variants or hook state to production types; follow the project's Rust best practices. Project automation remains Deno TypeScript; add no bash/Python project scripts.

5. D5. Remove all `ScanStats` mapping prose from parser error rustdoc and replace the README counter table with parser-only error documentation. Core already implements categorization, but `crates/adamite-core/src/stats.rs` lacks an equivalent mapping explanation: document actual behavior there, using `scan/ingest.rs::record_error`, `scan/ingest/decode.rs::{contribution_error,value_counter}`, merge counters and tree name decoding as evidence. Document implemented counters and fatal boot/bootstrap errors consistently with D3. Preserve downstream `Geometry` variant names when changing their source type to `BootError`.

6. D6. Rename integration tests `geometry.rs -> boot.rs`, `file_record.rs -> record.rs`, `attributes.rs -> attribute.rs`, `typed_values.rs -> value.rs`, and `file_ref.rs -> reference.rs`. Keep `attribute_list.rs` for the corresponding value module. Move direct-fixup tests needing internal access into one file-level `#[cfg(test)] mod tests` in `record/fixup.rs`, retaining their cases; integration record tests exercise the public pipeline. Move `tests/bytes.rs` assertions into the same form of test module in `src/util.rs`, removing the source-file inclusion. Preserve property regression paths. Final integration test files: `boot.rs`, `record.rs`, `reference.rs`, `attribute.rs`, `value.rs`, `attribute_list.rs`, `runlist.rs`, `utf16.rs`, `no_alloc.rs`, `budget.rs`, `fuzz_seeds.rs`, `fixtures.rs`, `error_size.rs`, `builders.rs`; retain `support/mod.rs` and `golden/`. Keep `no_alloc.rs` separate because it installs a global allocator.

Remove the fixup-token `compile_fail,E0308` doctest as the one intentional doctest removal. Retain and update the live-view `compile_fail,E0499` borrowing example to call `FileRecord::parse(&mut buffer)`. Existing atomicity properties cover fixup/classification; extend them to direct parse and add a runnable error-atomicity doctest. Add focused regressions for header-only, fixup-only and combined failures, downstream error categories/source chains, successful restoration, and unchanged classification results.

## Tasks

Each milestone is one bounded agent run of approximately 15–30 minutes, including verification; split further before implementation if needed. Stop at each boundary and present its diff and validation for human review. Commit only after explicit approval, using Conventional Commits. Commands run from the repository root.

### M1 — Record, boot and private helpers

- [x] Merge util helpers and relocate their existing tests; move record, fixup, references and system constants; rename boot/error; implement the record/iterator region boundary and shared-rule ownership in D2.
- [x] Keep temporary root re-exports, the old public operations/token and `GeometryError` alias so existing consumers compile through M2. Keep constant names temporarily. Finish with `deno task check` green.

### M2 — Attribute headers and typed values

- [x] Extract `attribute/iter.rs`, rename the descriptor file, move runlists under attribute and typed decoders/helpers/errors under value; relocate descriptor parsing as specified in D2 and apply D4 visibility.
- [x] Retarget temporary root re-exports to the moved items. Finish with `deno task check` green.

### M3 — Type-owned parsing and classification

- [x] Implement D1 associated operations and D2's shared record pipeline; restrict fixup/token visibility and migrate every operation call in core, fixtures, parser tests, both fuzz packages and examples. Keep temporary root type re-exports for M4.
- [x] Implement D3 error conversion, relocate direct-fixup tests, and perform D6 doctest changes and focused regressions. Finish with `deno task check` green.

### M4 — Public paths and downstream migration

- [x] Remove every root re-export/compatibility alias and rename system constants. Switch parser internals/tests, core source/unit tests, fixtures, both fuzz packages and all rustdoc/README examples to D1 module paths.
- [x] Audit scripts for stale source paths while retaining fuzz target/selector names. Finish with `deno task check` green.

### M5 — Test names, documentation and final verification

- [ ] Apply D6 test renames; update README/module documentation, the architecture crate description and core counter documentation per D5.
- [ ] Audit public items and dependency edges against D1–D2. Snapshot both fuzz corpora before smoke testing; restore only smoke-generated changes and verify original filenames and byte hashes afterward.
- [ ] Finish with both `deno task check` and `deno task check --fuzz` green; record commands and results in the milestone review.

## Acceptance criteria

- [ ] `rg -n '\bpub use\b' crates/adamite-ntfs/src` returns no matches; `lib.rs` has only the D1 contents, and generated rustdoc matches the public API table with one path per item.
- [ ] Inspect `rg -n '^pub fn|^    pub fn' crates/adamite-ntfs/src` and the broader `rg -n '^\s*pub\s+(const\s+|async\s+|unsafe\s+)*fn\b' crates/adamite-ntfs/src`: every public operation is inside a type's impl, with zero module-level public functions. Rustdoc exposes only `FixupError` under `record::fixup`; `apply` and `FixedRecord` are internal.
- [ ] List cross-module edges using `rg -n -U 'use\s+(crate|super|self)::[^;]*;' crates/adamite-ntfs/src`; also inspect fully qualified paths and moved impl blocks. Resolve relative imports and confirm every edge matches D2 and the graph has a topological ordering, including `#[cfg(fuzzing)]` code.
- [ ] Searches find no old `GeometryError`, `_RECORD` system names, public free operations, external fixup/token calls, root item imports or obsolete module paths in active source, tests, fuzz code and examples; parser source/README contain no `ScanStats` mapping prose.
- [ ] Direct-parse atomicity and D3 priority/category/source-chain regressions pass. Retain all existing cases and doctests except the token doctest, adapting API calls and expected errors where D3 changes precedence. Classification, allocation/error-size checks, fixture oracles and embedded builds pass. Both final gates are green; fixture files, fuzz selectors, target names and both corpora are unchanged.
