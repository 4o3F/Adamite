# Tasks

## 1. Workspace scaffolding

- [x] 1.1 Create the root `Cargo.toml` per design D1 (resolver 3, members `crates/*`, exclude `crates/adamite-ntfs/fuzz`; `[workspace.package]` edition 2024, rust-version 1.99, license `MIT OR Apache-2.0`; `[workspace.dependencies]` snafu with `default-features = false, features = ["rust_1_81"]`; baseline `[workspace.lints]`) and `rust-toolchain.toml` (channel 1.99, clippy, rustfmt). Verify `cargo metadata --format-version 1` succeeds and `rustup show active-toolchain` reports 1.99.
- [x] 1.2 Add `rustfmt.toml` (stable options only), `.gitattributes` (`tests/fixtures/** binary`, `*.json text eol=lf`), `.gitignore` (`target/`, `fuzz/artifacts/`, `fuzz/coverage/`), `LICENSE-MIT` and `LICENSE-APACHE` (copyright 4o3F). Verify the files exist and `git check-attr binary tests/fixtures/ntfs/x.img.zst` reports `set`.
- [x] 1.3 Create `crates/adamite-ntfs` with `publish = false`, features `default = []` and `alloc = []`, `[dependencies] snafu.workspace = true` as the only runtime dependency, and a `lib.rs` containing `#![no_std]`, `#![forbid(unsafe_code)]`, the strict `#![deny(...)]` set and crate docs. Verify `cargo build -p adamite-ntfs` succeeds with and without `--features alloc`, and `cargo tree -p adamite-ntfs -e normal` lists only `snafu` (and its derive crate).
- [x] 1.4 Add dev-dependencies `proptest` (`default-features = false, features = ["std"]`), `ruzstd`, `serde` (derive) and `serde_json`, and commit `Cargo.lock`. Verify `cargo test -p adamite-ntfs --locked` runs (0 tests), and `cargo build -p adamite-ntfs --target thumbv7em-none-eabihf` passes with and without `--features alloc` (install the target with rustup first).
- [x] 1.5 Add `deny.toml` (license allow list per D14, advisories and bans enabled, sources restricted to crates.io). Verify `cargo deny check` passes.
- [x] 1.6 Add the root `deno.json` (tasks `check`, `fixtures`, `fuzz-corpus`, `ci-lint`, each with least-privilege permission flags; fmt/lint config; `jsr:@std/*` imports) and `deno.lock`, and write `scripts/check.ts` running the D14 commands locally (with an optional `--fuzz` flag and a check that `rust-toolchain.toml` and `rust-version` agree). Verify `deno task` lists all four tasks, `deno fmt --check`, `deno lint` and `deno check scripts/` pass, and `deno task check` succeeds on the scaffold.

## 2. Byte readers, errors, FileRef and UTF-16

- [x] 2.1 Implement `src/bytes.rs` with checked little-endian readers (u8/u16/u32/u64/i8/i64 at an offset → `Option`), a checked `range(off, len)`, and alignment helpers. These are the only offset arithmetic in the crate. Verify `cargo clippy -p adamite-ntfs -- -D warnings` passes with the strict lint set.
- [x] 2.2 Implement `src/file_ref.rs`: `FileRef` (`from_raw`, `new` → `Result`, `raw`, `record`, `sequence`, `is_null`, `from_le_bytes`/`to_le_bytes`, a `Debug` showing `record:seq`) and the record constants. Add `tests/file_ref.rs` with the FileRef PBT properties from D12 and the spec scenarios. Verify `cargo test -p adamite-ntfs --test file_ref` passes.
- [x] 2.3 Implement `src/utf16.rs`: `Utf16Le` (odd-length rejection, `units`, `decode_strict`, `decode_lossy`, `len_units`), and alloc-gated `to_string_strict` and `to_string_lossy` using `try_reserve`. Add `tests/utf16.rs` comparing against `char::decode_utf16` (PBT) and the unpaired-surrogate scenarios. Verify it passes with and without `--features alloc`.
- [x] 2.4 Establish the snafu error conventions from D6 (`#[derive(Debug, Clone, Copy, PartialEq, Eq, Snafu)]`, `#[snafu(visibility(pub(crate)))]`, `#[non_exhaustive]`, a `display` message per variant, `source` only for Copy parser errors, rustdoc stating the ScanStats mapping), applied first to `FileRefError` and `Utf16Error`. Add `tests/error_size.rs` asserting `size_of` ≤ 24 and `core::error::Error + Copy` bounds for every public error enum, extending it as each enum lands. Verify the test passes and the thumbv7em build still succeeds.

## 3. Volume geometry

- [x] 3.1 Add `BootSectorBuilder` to `tests/support/builders.rs`, encoding both record-size encodings and the exponent cluster encoding without calling production code.
- [x] 3.2 Implement `src/geometry.rs`: `parse_boot_sector`, `VolumeGeometry` accessors (including `total_clusters` and `total_bytes`), `cluster_byte_range`, and `GeometryError`, following every rule in `specs/ntfs/volume-geometry`. Rustdoc each accessor with its on-disk offset.
- [x] 3.3 Add `tests/geometry.rs` covering every spec scenario, golden vectors (a real Windows boot sector, an mkntfs boot sector, 0xF4 and 0xF8 exponent cases), and the geometry PBT properties from D12 (round trip, exhaustive `sectors_per_cluster` / record-size bytes, suffix independence, truncation, `cluster_byte_range` bounds). Verify `cargo test -p adamite-ntfs --test geometry` passes.

## 4. FILE record header, fixup and classification

- [x] 4.1 Add `RecordBuilder` to `tests/support/builders.rs`, with 3.0/3.1 headers, attribute assembly, USA protection using a chosen USN, and the ability to corrupt any one tail.
- [x] 4.2 Implement `RecordHeader::peek` and `RecordFlags` in `src/record.rs` (layout detection by `usa_offset ≥ 0x30`, `record_number() -> Option<u32>`, unknown flag bits kept). Add peek scenarios to `tests/file_record.rs`. Verify they pass.
- [x] 4.3 Implement `src/fixup.rs`: two-pass `apply(&mut [u8]) -> Result<FixedRecord, FixupError>` with the 512-byte stride, exact `usa_count`, USA bounds (≤ 510 and ≤ `first_attr`), any USN accepted, and no writes on error. Add fixup scenarios plus the PBT properties (round trip, atomicity, count/offset boundaries, 512 B / 1 KiB / 4 KiB / 64 KiB sizes). Verify they pass.
- [x] 4.4 Implement `FileRecord::parse(FixedRecord)`, header validation and `classify(&mut [u8]) -> RecordClass` with the D3 precedence (NotInUse is not fixed up). Add classification scenarios and the classification-model PBT, including the base-ref (0, seq ≠ 0) case and the 3.0 sentinel-bytes-at-0x2C case. Verify they pass.
- [x] 4.5 Add a `compile_fail` doctest on `FileRecord` showing that mutating the buffer while a view is alive does not compile. Verify `cargo test -p adamite-ntfs --doc` passes.

## 5. Runlist

- [x] 5.1 Add `RunlistEncoder` to `tests/support/runlist_encoder.rs`, supporting minimal and padded field widths and sparse runs, independent of production code.
- [x] 5.2 Implement `src/runlist.rs`: `VcnRange::new` (empty-stream −1 rule, `highest_vcn = 0` literal), the fused `Runlist` iterator (unsigned lengths, sign-extended deltas, sparse runs keep the base, per-segment reset, checked arithmetic, exact coverage, missing terminator, padding ignored), and `Run::validate_geometry`. Add the alloc-gated `collect_runs` helper.
- [x] 5.3 Add `tests/runlist.rs` covering every spec scenario, golden vectors (including `21 18 34 56 00` and a negative-delta vector from the NTFS docs), and the runlist PBT properties from D12 (round trip, width independence, sparse base, segment split, coverage ±2, overflow and negative LCN, bound of n/2 items, fused). Verify `cargo test -p adamite-ntfs --test runlist` passes.

## 6. Attribute iteration

- [x] 6.1 Add `AttrBuilder` (resident and non-resident, names, flags, extended header) to `tests/support/builders.rs`.
- [x] 6.2 Implement `src/attribute/mod.rs`: the fused `AttributeIter` bounded to `[first_attr, bytes_used)`, the end marker, length rules (8-aligned, ≥ 0x18 / ≥ 0x40), type 0 rejected, non-resident flag ∈ {0, 1}, name bounds, `AttributeType` and `AttributeFlags` constants, and `Attribute` accessors. Wire `FileRecord::attributes()`.
- [x] 6.3 Implement `src/attribute/nonresident.rs`: `NonResident` (lowest/highest VCN, `compression_unit` from the u8 at 0x22, mapping pairs, `runs()` returning a `Runlist`), and `sizes() -> Option<StreamSizes>` (only when `lowest_vcn == 0`, non-negative, `initialized ≤ data`, `compressed_size` only when the header is ≥ 0x48 and the attribute is compressed or sparse).
- [x] 6.4 Add `tests/attributes.rs` covering every attribute-iteration and descriptor scenario from `specs/ntfs/file-record`, plus the D12 attribute PBT properties (round trip, `bytes_used` boundary, slack and post-marker invariance, progress/fused, continuation-segment size masking, reserved bytes 0x23..0x28). Verify it passes.
- [x] 6.5 Add `tests/no_alloc.rs` with a counting `#[global_allocator]`. It classifies RecordBuilder records and exhausts their attributes and runlists, asserting zero allocations. Verify it passes with and without `--features alloc`.

## 7. Typed attribute decoders

- [x] 7.1 Implement `$STANDARD_INFORMATION` (48-byte, or ≥ 72 bytes with the tail ignored; `FileTimes`; `FileAttributes` constants; extended fields) and `$FILE_NAME` (≥ 0x42 + 2n, n 1..=255, namespace 0..=3, all fields, `Utf16Le` name). Both reject a wrong type or storage form. Add scenarios and PBT to `tests/typed_values.rs`. Verify they pass.
- [x] 7.2 Implement `$DATA` (name, flags, resident value or non-resident descriptor) and `$INDEX_ALLOCATION` (must be non-resident; name, flags, descriptor). Add scenarios, including a named and an unnamed stream in one record. Verify they pass.
- [x] 7.3 Implement `$REPARSE_POINT` (8..=16384 bytes, `8 + data_len ≤ len`, tag, opaque payload) and the `Bitmap` view (`logical_bits` ≤ capacity, LSB-first `get`, fused `iter`, `iter_set`). Add scenarios and PBT against a naive bitmap model. Verify they pass.
- [x] 7.4 Implement the `$ATTRIBUTE_LIST` entry iterator (value ≤ 262144 bytes, entry ≥ 0x1A within the value, a short all-zero tail ends iteration, non-negative `lowest_vcn`, name bounds, fused) and add `AttrListBuilder`. Add scenarios and the entry round-trip and truncation PBT. Verify they pass.
- [x] 7.5 Add a "no scanner policy" test (conflicting cached `$FILE_NAME` size vs `$DATA` size, both reported raw) and a totality PBT feeding arbitrary bytes from 0 to 65536 long to every typed decoder. Verify they pass.

## 8. Real-image fixtures

- [x] 8.1 Write `scripts/fixtures/lib.ts` (a `Deno.Command` runner that fails on a non-zero exit, a temp-dir and mount guard with `finally` unmount) and `scripts/fixtures/populate/{basic-4k,frag-512,4kn-64k}.ts` with exactly the contents in design D10, including the frag-512 postcondition checks and up to 3 retries. Verify `deno check` and `deno lint` pass for `scripts/fixtures/`.
- [x] 8.2 Write `scripts/fixtures/manifest.ts` (`collect` from the mount via `Deno.lstat` and `getfattr`; `finalize` from the image via `ntfsinfo`), emitting the D10 schema with large u64 values as strings. It must not use adamite-ntfs. Verify `deno check` passes, and a unit test (`deno test scripts/fixtures/`) checks the u64-as-string serialization and the schema shape against a sample.
- [x] 8.3 Write `scripts/fixtures/generate.ts` implementing the D10 per-image steps (tool check, `Deno.truncate`, `mkntfs -F -Q -q -T -s/-c`, `sudo ntfs-3g` with `streams_interface=windows`, populate, collect, `finally` unmount, clean-volume check, finalize, `zstd -19`, move outputs, print sizes against the budget), and wire it to `deno task fixtures [name]` with the D10 permission flags. Verify `deno task fixtures --help` prints usage without needing sudo.
- [x] 8.4 **(User action, needs sudo)** Run `sudo apt install ntfs-3g zstd attr`, then `deno task fixtures`, and commit `tests/fixtures/ntfs/{basic-4k,frag-512,4kn-64k}.img.zst` and `.json`. Verify the task exits 0, all postconditions pass, and the total `.zst` size is ≤ 1 MiB.

The user completed generation and verification. Committing the fixtures is deferred until human review, as explicitly authorized by the orchestrator.
- [x] 8.5 Implement `tests/support/fixture.rs` (decompress with ruzstd and check `image_len`, parse the manifest with `deny_unknown_fields`, read record 0, stitch `$MFT` via the extents and the attribute list if present, iterate records). Add `tests/budget.rs` enforcing the 1 MiB limit. Verify both compile and `budget` passes.
- [x] 8.6 Add `tests/fixtures.rs` with the deterministic oracles from D12: geometry, `$MFT` extents, every entry's seq/flags/names/streams/reparse/attribute-list presence and mtime, the deleted record's `NotInUse`, root parent 5:5, and zero `Corrupt` across every stitched MFT. Verify `cargo test -p adamite-ntfs --test fixtures` passes on Linux, and on Windows via CI.

## 9. Fuzzing

- [x] 9.1 Create `crates/adamite-ntfs/fuzz` (its own `[workspace]`, `libfuzzer-sys`, path dependency with `features = ["alloc"]`, its own `Cargo.lock`) with the five targets in D13, each exhausting every iterator and calling every accessor. Verify `cargo +nightly fuzz build` succeeds after `cargo install cargo-fuzz --locked`.
- [x] 9.2 Write `scripts/fuzz_corpus.ts` (`deno task fuzz-corpus`) to extract seed corpora (≤ 64 KiB per target) from the fixtures into `fuzz/corpus/<target>/`, decompressing with `zstd -d` via `Deno.Command`. Verify the corpora are produced within budget, and each target runs for 30 s with `-max_total_time=30` and no crash.
- [x] 9.3 Convert any crash found in 9.2 into a deterministic regression test in the matching `tests/*.rs` file, and fix the cause. Verify the fuzz smoke runs for all five targets are clean and the regression tests pass.

All five task 9.2 smoke runs completed 30 seconds without a crash; task 9.3 required no crash-derived regression.

## 10. CI

- [x] 10.1 Write `.github/workflows/ci.yml` with the `check` (ubuntu + windows), `no_std`, `deny`, `deno` and `fuzz` jobs exactly as in design D14. Verify `deno task ci-lint` parses it, and `deno task check` passes locally on Linux, running every non-fuzz command in the file.

## 11. Documentation

- [x] 11.1 Apply the docs/ARCHITECTURE.md corrections listed in design D15, including the new Conventions section. Verify by re-reading §2.2–§3.1 against `specs/ntfs/*` and the Conventions section against `openspec/config.yaml`, so that no contradictions remain.
- [x] 11.2 Write the `crates/adamite-ntfs` crate-level rustdoc and README: purpose, the no-I/O and no-logging contract, the Phase 1 calling pattern (peek → classify → attributes → copy out → drop views), the leniency profile (D7), the snafu error types and their ScanStats mapping, and how to regenerate fixtures (`deno task fixtures`) and run fuzzing (`deno task fuzz-corpus`, `cargo +nightly fuzz run`). Verify `cargo doc -p adamite-ntfs --no-deps --all-features` with `RUSTDOCFLAGS=-D warnings` passes, and that README code blocks are valid.

## 12. Integration verification

- [x] 12.1 Run the full local gate with `deno task check --fuzz`: cargo fmt, clippy with both feature sets (`-D warnings`), `cargo test --workspace --locked` with both feature sets and `PROPTEST_CASES=1024`, the thumbv7em `no_std` build, `cargo deny check`, Deno fmt/lint/check plus `ci-lint`, and a 30 s fuzz smoke on all five targets. Verify the task exits 0.
- [x] 12.2 Run `openspec validate add-ntfs-parser --strict` and confirm every spec scenario maps to at least one named test (record the mapping in the change's final review notes). Verify validation passes and no scenario is unmapped.

## Workflow follow-up

- Run `/ccg:spec-review` for dual-model review after implementation.
- Archive the change after review approval and after the commit has been reviewed by the user.
