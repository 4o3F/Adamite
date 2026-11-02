# Design

## Context

The repository contains only docs/ARCHITECTURE.md. This change creates the Cargo workspace and the first crate, `adamite-ntfs`. Motivation is in proposal.md; behavior contracts are in `specs/ntfs/*`.

Constraints from the architecture:

- **Phase 1 hot path:** workers own pooled chunk buffers (`&mut [u8]`, record-aligned) and must process ~10M records with zero per-record heap allocation.
- **Parallelism:** the core never uses rayon's global pool, so the parser must be stateless and thread-agnostic.
- **Hostile input:** live volumes yield torn and garbage records, so every byte is untrusted.

Project conventions (`openspec/config.yaml`):

- Toolchain pinned to Rust 1.99.
- snafu for errors.
- tracing for logging (not in this crate).
- Deno TypeScript for every script and task.

Local environment: Rust 1.99 plus nightly and Deno 2.9 are installed; `cargo-fuzz`, `mkntfs` and `ntfs-3g` are not; sudo needs a password.

## Goals / Non-Goals

**Goals:**

- A `#![no_std]`, `#![forbid(unsafe_code)]` parser whose only runtime dependency is `snafu`. No panic on any input is enforced statically (clippy deny lints) and dynamically (proptest and fuzzing).
- An API shaped for the core's Phase 0–2 calling pattern (see Decisions D2–D4).
- Tests that run identically on Linux and Windows with no system tools or privileges.

**Non-Goals:**

- Reading devices or images (the future `adamite-io` crate).
- Stitching MFT extents, merging extension records, choosing names, size accounting (`adamite-core`).
- Parsing INDX blocks, decompressing streams, interpreting reparse targets, `$LogFile`, `$UsnJrnl`, or `$Secure`.
- Creating placeholder crates for the rest of the workspace.
- Logging. The parser reports everything through return values; `tracing` instrumentation starts in `adamite-core`.

## Decisions

### D1. Workspace and crate layout

```
Cargo.toml                # [workspace] resolver="3", members=["crates/*"], exclude=["crates/adamite-ntfs/fuzz"]
                          # [workspace.package] edition="2024", rust-version="1.99", license="MIT OR Apache-2.0"
                          # [workspace.dependencies] snafu = { version = <latest>, default-features = false,
                          #                                    features = ["rust_1_81"] }
                          # [workspace.lints] baseline: rust.unsafe_code="deny", rust.missing_docs="warn",
                          #                   rust.rust_2018_idioms="warn", clippy.all="warn"
rust-toolchain.toml       # [toolchain] channel="1.99", components=["clippy","rustfmt"]
deno.json  deno.lock      # tasks: check, fixtures, fuzz-corpus; fmt/lint config; imports: jsr:@std/*
Cargo.lock  rustfmt.toml  deny.toml  .gitattributes  LICENSE-MIT  LICENSE-APACHE
.github/workflows/ci.yml
crates/adamite-ntfs/
  Cargo.toml              # publish=false, [features] default=[] alloc=[], [lints] workspace=true,
                          # [dependencies] snafu.workspace = true
  src/lib.rs              # #![no_std], cfg(feature="alloc") extern crate alloc, strict #![deny(...)]
  src/bytes.rs            # private: checked LE readers, checked ranges (the only place offsets are computed)
  src/iter.rs             # private: shared fused-step completion for fallible iterators
  src/geometry.rs         # VolumeGeometry, GeometryError
  src/file_ref.rs         # FileRef, FileRefError, record constants
  src/record.rs           # RecordHeader (peek), RecordFlags, FileRecord, RecordClass, RecordError
  src/fixup.rs            # apply(), FixedRecord<'a>, FixupError
  src/attribute/mod.rs    # AttributeIter, Attribute, AttributeType, AttributeFlags, AttributeError
  src/attribute/nonresident.rs   # NonResident<'a>, StreamSizes
  src/attribute/{standard_information,file_name,data,index_allocation,reparse_point,bitmap,attribute_list}.rs
  src/runlist.rs          # VcnRange, Run, Runlist<'a>, RunlistError
  src/utf16.rs            # Utf16Le<'a>, Units, StrictChars, LossyChars, Utf16Error; alloc-gated String helpers
  tests/*.rs              # all tests are integration tests (see D9)
  tests/support/{mod,builders,runlist_encoder,fixture}.rs
  fuzz/                   # separate cargo-fuzz package with its own [workspace] and Cargo.lock
tests/fixtures/ntfs/      # *.img.zst + *.json (workspace root, reusable by adamite-core later)
scripts/check.ts          # local gate mirroring CI (deno task check)
scripts/fixtures/         # generate.ts, populate/{basic-4k,frag-512,4kn-64k}.ts, manifest.ts, lib.ts
scripts/fuzz_corpus.ts    # seed-corpus extraction (deno task fuzz-corpus)
```

The workspace denies unsafe code so task 6.5's test-only counting allocator can allow it locally on its `GlobalAlloc` implementation, while `adamite-ntfs/src/lib.rs` continues to forbid unsafe code.

Upgrading the toolchain means editing both `rust-toolchain.toml` and `rust-version`. There is no separate MSRV: the crate is `publish = false` and has no external consumers.

The strict lint set (`clippy::indexing_slicing`, `arithmetic_side_effects`, `unwrap_used`, `expect_used`, `panic`) is a crate-level `#![deny]` in `adamite-ntfs/src/lib.rs`. It is not in `[workspace.lints]`, because Cargo cannot combine `workspace = true` with per-crate overrides, and core's arena indexing should not be bound by it.

*Alternative rejected:* putting everything in `[workspace.lints]`. That forces the strict set onto every future crate.

### D2. Fixup in place, with a token type

`fixup::apply(&'a mut [u8]) -> Result<FixedRecord<'a>, FixupError>` runs in two passes:

1. Validate all metadata and every tail. Any failure returns an error with zero writes.
2. Restore the tails.

`FixedRecord<'a>` has a private constructor and holds the reborrowed `&'a [u8]`. `FileRecord::parse(FixedRecord<'a>)` is the only path to attributes. This prevents applying fixup twice: after restoration a tail may happen to equal the USN, and a second pass would silently "succeed" and corrupt the record. The borrow checker also guarantees that no view outlives the moment the worker returns its buffer to the pool.

*Alternatives rejected:*

- **Copying each record into an owned buffer:** about 10 GB of extra copying at 10M records.
- **A virtual overlay that patches tails on read:** every reader becomes tail-aware, and payloads crossing a tail cannot be borrowed as plain slices.

### D3. Peek-then-classify for the hot path

`RecordHeader::peek(&[u8])` decodes the fixed header (bytes < 0x30, which lie before the first tail at 0x1FE) without mutation. `classify(&'a mut [u8]) -> RecordClass<'a>` works in this order:

1. Size check.
2. Uniform 0x00 or 0xFF → `Unused`.
3. `BAAD` → `Baad`.
4. Non-`FILE` → `Corrupt`.
5. Header validation.
6. `!IN_USE` → `NotInUse(RecordHeader)`, with **no fixup**.
7. Fixup.
8. `Base(FileRecord)` or `Extension(FileRecord)`.

Large free regions of the MFT therefore cost one header read per record. The `$MFT:$BITMAP` check in the core skips most of them earlier still.

*Alternative rejected:* validating fixup for not-in-use records (codex's suggestion). It adds work on the dominant record class, and the core never reads their attributes.

### D4. Header layout inferred from `usa_offset`

`usa_offset ≥ 0x30` means NTFS 3.1 and exposes `record_number() -> Option<u32>`. Anything else means 3.0, where `record_number()` returns `None`. The boot sector does not carry the NTFS version, and the scanner should not need `$Volume` before parsing records.

*Alternative rejected:* a caller-supplied layout enum. It is an extra parameter on every hot-path call and has no practical benefit.

### D5. Eager headers, borrowed bodies

Fixed-size headers (`RecordHeader` ~40 B, attribute header fields, `StandardInformation`) are decoded once into `Copy` structs, so accessors are infallible field reads. Variable-length data (names, resident values, mapping pairs, reparse payloads, attribute-list values) stays as `&'a [u8]` views. Public structs with public fields are `#[non_exhaustive]`; the rest use accessor methods. Names are `Utf16Le<'a>` over raw bytes (possibly unaligned), never `&[u16]`.

The alloc-gated fallible runlist collector is named `Runlist::collect_runs` so `Iterator::collect` has the same meaning with either feature set.

### D6. Errors

Each module has one error enum: `GeometryError`, `FileRefError`, `FixupError`, `RecordError` (wraps `FixupError`), `AttributeError`, `ValueError`, `RunlistError`, `BitmapError`, `AttributeListError`, `Utf16Error`.

- Every enum is defined with `#[derive(Debug, Clone, Copy, PartialEq, Eq, Snafu)]`, `#[snafu(visibility(pub(crate)))]` and `#[non_exhaustive]`.
  - Each variant has a `#[snafu(display(...))]` message.
  - snafu (with `rust_1_81`, no std or alloc) generates `Display` and `core::error::Error`.
  - Internal code raises errors through context selectors: `ensure!(cond, XxxSnafu { .. })` and `OptionExt::context` on the `Option`s returned by `bytes.rs`.
- Wrapping, e.g. `RecordError::Fixup { source: FixupError }`, is allowed only with another `Copy` parser error as `source`. Errors never contain backtraces, `Whatever`, `String` or boxed sources, so every enum holds no borrowed or heap data and is at most 24 bytes (asserted by a test).
- `AttributeListError::Value { source: ValueError }` preserves the shared typed-decoder type/storage errors and their ScanStats mappings.
- Validation order is fixed and documented: input extent → signature/discriminant → declared lengths → sub-offsets → numeric relationships. Given an input, the reported error is therefore deterministic.
- `ScanStats` counter mapping is documented in each enum's rustdoc. The core owns the counters.

### D7. Leniency profile (user decision: tolerant)

Memory safety, bounds, iterator progress and fixup integrity are always enforced. Format deviations that are harmless are accepted:

- `$STANDARD_INFORMATION` of exactly 48 bytes, or of 72 bytes or more (trailing bytes ignored).
- `$FILE_NAME` value length ≥ 0x42 + 2n.
- Any USN value.
- Unknown attribute types, flag bits and reparse tags.
- An `$ATTRIBUTE_LIST` tail of fewer than 0x1A zero bytes.
- No required alignment for resident value offsets or mapping-pairs offsets.

Attribute lengths still must be 8-aligned: that guarantees progress and that every attribute starts aligned.

### D8. `$ATTRIBUTE_LIST` scope

A minimal entry decoder (type, lowest_vcn, segment `FileRef`, id, name) is included, so the core can bootstrap a `$MFT` whose `$DATA` spans extension records. Resolving those records, detecting cycles and assembling extents stay in the core. Phase 1 still merges extension records by `base_ref` and never reads attribute lists.

### D9. Tests: real images plus synthetic builders

- **Location:** all tests live in `crates/adamite-ntfs/tests/`. Each file starts with `#![allow(clippy::indexing_slicing, clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::arithmetic_side_effects)]`. `lib.rs` has no `#[cfg(test)]` modules.
- **Dev-dependencies:** `proptest` (`default-features = false, features = ["std"]`), `ruzstd`, `serde` (derive) and `serde_json`. There is no `sha2`: integrity relies on the zstd frame checksum plus a decompressed-length check.
- **Independent encoders (`tests/support/builders.rs`):** `BootSectorBuilder`, `RecordBuilder` (3.0/3.1 headers, attribute assembly, USA protection), `AttrBuilder` (resident/non-resident), `FileNameBuilder`, `AttrListBuilder`, `RunlistEncoder` (minimal and padded widths). These never call the production decoders.
- **Golden vectors:** at least 10 hand-written byte vectors taken from Microsoft documentation and NTFS-3G/ntfs3 comments, so that an encoder and decoder sharing the same bug cannot hide it.
- **Allocation check:** a counting `#[global_allocator]` lives in its own test binary, `tests/no_alloc.rs`.
- **Fixture helper (`tests/support/fixture.rs`):** decompresses an image (checking `image_len`), reads record 0, stitches `$MFT` from its extents (following `$ATTRIBUTE_LIST` only through extents already known), and yields record slices. It is test-only and marked as the reference behavior for `adamite-core` Phase 0.

### D10. Fixture images

Fixtures are generated once by `deno task fixtures` (Linux, sudo, `ntfs-3g` + `zstd` + `attr`) and committed. Tests never mount or invoke tools.

**Generator layout** (Deno TypeScript):

- `scripts/fixtures/generate.ts` is the entry point (all images, or one if a name is given).
- `scripts/fixtures/populate/<image>.ts` exports the population steps for each image.
- `scripts/fixtures/manifest.ts` produces the manifest:
  - `collect` from the mount: `Deno.lstat` for ino, nlink and size; `getfattr` through `Deno.Command`;
  - `finalize` from the image: `ntfsinfo`.
- `scripts/fixtures/lib.ts` holds the shared command runner and cleanup.

The task passes only least-privilege flags:

- `--allow-run=mkntfs,sudo,ntfsinfo,getfattr,setfattr,zstd,mountpoint`;
- `--allow-read` and `--allow-write` limited to `tests/fixtures/ntfs` and the temp directory.

**Per-image steps:**

1. Check that the tools exist.
2. Create a temp dir with `Deno.makeTempDir`, plus a `try/finally` cleanup that unmounts and removes it.
3. Create a sparse image of SIZE bytes with `Deno.truncate`.
4. Run `mkntfs -F -Q -q -T -s SECTOR -c CLUSTER -L ADAMITE_<NAME>`.
5. Mount with `sudo ntfs-3g -o streams_interface=windows,uid,gid`.
6. Populate the image.
7. Collect manifest values from the mount.
8. Run `sync`, then `sudo umount`, and wait until the mountpoint is gone.
9. Check that `ntfsinfo` reports the volume as clean.
10. Finalize the manifest from the image.
11. Compress with `zstd -19`.
12. Move the outputs into `tests/fixtures/ntfs/` and print the sizes against the budget.

**Images**

| Image | Size | Sector / cluster / record | Purpose |
|---|---|---|---|
| `basic-4k` | 16 MiB | 512 / 4096 / 1024 (`clusters_per_record` 0xF6) | Content coverage |
| `frag-512` | 32 MiB | 512 / 512 / 1024 (`clusters_per_record` +2; records straddle clusters) | Fragmentation |
| `4kn-64k` | 64 MiB | 4096 / 65536 / 4096 (`usa_count` 9) | Large-sector geometry |

**Contents of `basic-4k`** (all mtimes are set to 2020-01-01T00:00:00Z = FILETIME 132223104000000000):

- `/empty.txt`: 0 B.
- `/tiny.txt`: 100 B, resident.
- `/medium.bin`: 64 KiB, byte i = i mod 251.
- `/bigdir/f000`…`f199`: forces `$INDEX_ALLOCATION`.
- Hard links: `/hl/orig.txt`, `/hl/link-same-dir.txt` and `/other/link-other-dir.txt` (nlink 3, two parents).
- `/ads.txt` with streams `:meta` (32 B) and `:big` (20 KiB, non-resident), created with `streams_interface=windows`.
- Unicode names: `日本語.txt`, `emoji-😀.txt`, and one name of exactly 255 UTF-16 units.
- A 40-level `d00/…/d39/leaf.txt` path.
- `/sparse.bin`: 1 MiB logical, with 4 KiB written at offsets 0 and 512 KiB.
- `/compressed/c.txt`: 256 KiB of text, with the compression attribute set through `system.ntfs_attrib_be`.
- `/reparse.bin` with an explicit `IO_REPARSE_TAG_SYMLINK` (0xA000000C) buffer set through `system.ntfs_reparse_data`.
- `/LongFileName.txt` with DOS name `LONGFI~1.TXT` set through `system.ntfs_dos_name`.
- `/hidden.txt` with HIDDEN|SYSTEM.
- `/deleted.txt`, created then removed; its record number is captured first.

**Contents of `frag-512`:**

1. Fill the volume with 1-cluster files, then delete every other one.
2. Write `/frag.bin`, sized to consume about 600 holes. This forces a `$DATA` attribute list with extension records, including a segment with `lowest_vcn > 0`.
3. Create 1500 small files to grow `$MFT` into the holes.

The recipe **fails** unless `ntfsinfo` shows ≥ 2 extents for `$MFT` and an attribute list on `/frag.bin`. A `$MFT` that itself has an attribute list is recorded in the manifest if it occurs, but it is not required; synthetic records cover it.

**Contents of `4kn-64k`:** the root directory, `/a.txt` (100 B) and `/b.bin` (200 KiB).

**Budget.** The sum of `*.img.zst` files is ≤ 1 MiB (compressed with `zstd -19`, no `--long`), enforced by `tests/budget.rs`.

**Manifest.** One `<image>.json` per image with:

- `schema_version: 2`;
- `image`, `image_len`, generator tool versions;
- `geometry`;
- `mft` (`extents`, `data_size`, `has_attribute_list`);
- `entries[]` with `path`, `record`, `seq`, `kind`, `nlink`, `attributes`, `mtime_filetime`, `names[]` (`parent_record`, `parent_seq`, `name` as a UTF-16 unit array, `namespace`), `streams[]` (`name`, `size`, `resident`, `sparse`, `compressed`, `run_count`), `reparse_tag`, `has_attribute_list`;
- `deleted[]`.

**Manifest amendment (orchestrator decision).** Keep full `entries[]` for meaningful files and directories. Describe disposable `/fill/*` and `/growth/*` files with one object per directory in `fill[]`: `dir`, `count`, `name_prefix`, `name_len`, `record_min`, `record_max`, `parent_record`, and `parent_seq`; record numbers remain decimal strings. Filler oracles scan each inclusive record range, select names by the parent reference, and check Base classification, name length/prefix, exact count, and absence of corruption. Schema version 2 is mandatory and Rust retains `deny_unknown_fields`. Write numeric arrays on one line and each entry on one line, with a 256 KiB limit per manifest enforced by `tests/budget.rs`. The deterministic converter and future generation use the same summarization and serializer; conversion does not change image bytes.

u64 values that can exceed 2^53 are JSON strings. Values come from **independent tools only**: `stat` on the ntfs-3g mount (st_ino = record number), `getfattr`, and `ntfsinfo`. They are never produced by adamite-ntfs. Rust parses manifests with `deny_unknown_fields`.

**Determinism.** Regeneration is not byte-reproducible (mkntfs randomizes serials and GUIDs). An image and its manifest are always replaced together, and tests assert only on manifest values.

`.gitattributes` contains `tests/fixtures/** binary` and `*.json text eol=lf`.

### D11. Coverage that images cannot provide

The following are covered only by synthetic builders and golden vectors:

- NTFS 3.0 headers.
- `$STANDARD_INFORMATION` v1 (48 B).
- Exponent-encoded clusters (0xF4–0xFF).
- 1024/2048-byte sectors.
- Invalid UTF-16 (`ntfs-3g` refuses it).
- Empty non-resident streams (`highest_vcn = −1`).
- `$MFT` with an attribute list.
- `BAAD` records.
- All corruption cases.

### D12. Property-based testing

Settings are 256 cases locally and `PROPTEST_CASES=1024` in CI. `proptest-regressions/` is committed. Generators build a semantic model first and serialize it with the independent builders. Invalid cases mutate exactly one field of a valid model, and value choices are weighted toward boundaries (0, 1, min−1, min, max, max+1, powers of two, sign-width transitions). Test oracles use u128/i128 arithmetic.

| Area | Invariant | Falsification strategy |
|---|---|---|
| Geometry | `parse(encode(g)) == g` for valid g | Generate sector/cluster/record combos using both encodings; compare against a u128 model |
| Geometry | Accepts exactly the documented sets | Exhaust all 256 values of `sectors_per_cluster` and the record-size bytes; set-membership oracle |
| Geometry | Suffix independence; truncation always errs | Append random suffixes; truncate at every length below 512 |
| Geometry | `cluster_byte_range` stays in the volume, no overflow | LCN/count pairs near `total_clusters` and u64::MAX |
| FileRef | `from_raw(x).raw() == x`; `new` rejects ≥ 2^48 | `any::<u64>()` weighted at bits 47, 48, 63 |
| Fixup | `apply(protect(R, u)) == R` outside the USA | RecordBuilder for 512 B–64 KiB records, any u, tails equal to u |
| Fixup | Any error leaves the buffer byte-identical | Corrupt one tail (including the last one), or the count/offset |
| Fixup | Only the exact count and in-range USA are accepted | Offsets near 0x2A/0x30/first_attr/510; count ±1 |
| Record | `peek` agrees with the post-fixup header | Random flags, base_ref and seq |
| Record | Classification follows the documented precedence | Cross-product of signature × fill × fixup validity × IN_USE × base_ref, against a model |
| Record | 3.0 records never read 0x2C | Place sentinel bytes at 0x2C with `usa_offset` 0x2A |
| Attributes | Valid sequences round-trip in order (type, name, id, flags, value) | Mixed resident/non-resident and unknown types |
| Attributes | Never read past `bytes_used` or after the end marker | Shrink `bytes_used` across every field boundary; randomize slack |
| Attributes | Progress ≥ 24 B per item; fused | Zero, short, unaligned and wrapping lengths; call `next()` 16 more times |
| Attributes | `sizes()` is `None` iff `lowest_vcn > 0`; `compression_unit` reads 0x22 only | Random size bytes in continuation segments; random 0x23..0x28 |
| SI / FILE_NAME | Fields round-trip; length rules hold | Lengths 0..=100 and name length 0..=256; namespaces 0..=5 |
| UTF-16 | Units are preserved; strict and lossy match `char::decode_utf16` | `vec(any::<u16>())` weighted toward surrogates |
| alloc | String helpers equal the concatenated iterator output | Same generator with `--features alloc` |
| Reparse | Tag and payload preserved; length rules hold | Value lengths near 8, 8+n and 16384 |
| Bitmap | `get(n)` matches a naive model; iteration length = `logical_bits` | Random bytes × random `logical_bits` ≤ capacity |
| Attr list | Entries round-trip; progress; truncation errs | `vec(entry)` encoded, then cut at a random point |
| Runlist | `decode(encode(runs)) == runs` | Run lists with sparse runs and nonmonotonic LCNs, minimal widths |
| Runlist | Width independence | Re-encode each field with a padded width up to 8 |
| Runlist | Sparse runs keep the base; each segment resets | Sequences A, sparse, B; split a stream at every run boundary |
| Runlist | Exact coverage; overflow/negative LCN errs; fused | Declared `highest_vcn` ±2; deltas near the i64 limits |
| Runlist | Padding after the terminator is ignored | Append random suffixes |
| All | No panic on arbitrary input | `vec(any::<u8>(), 0..=65536)` into every public entry point, exhausting every iterator |

The fixture oracles are deterministic tests over every manifest entry: geometry, `$MFT` extents, seq/flags, `$FILE_NAME` sets including hard links and the DOS name, stream sizes, sparse/compressed flags, reparse tag, `$INDEX_ALLOCATION` presence, mtimes, the deleted record's `NotInUse` state, and that every record in each stitched MFT classifies with no `Corrupt`.

### D13. Fuzzing

`crates/adamite-ntfs/fuzz` contains `libfuzzer-sys` and its own `[workspace]` and `Cargo.lock`, and builds on nightly only. It has five targets:

- `boot_sector`;
- `file_record` (classify, then exhaust attributes and decode every typed value);
- `attributes` (raw region fed straight to `AttributeIter`);
- `runlist` (the first 16 bytes select the VCN range);
- `typed_values` (the first byte selects the decoder).

A seed corpus of ≤ 64 KiB per target, extracted from the fixtures by `deno task fuzz-corpus` (`scripts/fuzz_corpus.ts`), is committed under `fuzz/corpus/`. The fuzz crate uses the same `snafu` configuration through its path dependency, so the snafu-derive proc-macro adds to fuzz build time but not to the instrumented code.

The documented constants in `fuzz/fuzz_targets/typed_values/selectors.rs` define the typed-value input selectors. The Rust target and `tests/fuzz_seeds.rs` import that module; the Deno extractor reads its decimal constants by name. The seed test requires every selector to be represented and checks each generated seed with its intended Rust decoder.

### D14. CI (`.github/workflows/ci.yml`)

The workflow triggers on push to `main` and on `pull_request`, with `permissions: contents: read`, cancel-in-progress concurrency, and `Swatinem/rust-cache`. Rust jobs other than `fuzz` take their toolchain from `rust-toolchain.toml` (rustup picks it up automatically). Jobs:

- **`check`** (ubuntu-latest + windows-latest, pinned 1.99):
  - `cargo clippy --workspace --all-targets --locked -- -D warnings`, once with `--no-default-features` and once with `--all-features`;
  - `cargo test --workspace --locked` with both feature sets (`PROPTEST_CASES=1024`);
  - `cargo doc --workspace --no-deps --all-features` with `RUSTDOCFLAGS=-D warnings`;
  - Ubuntu only: `cargo fmt --all --check` and `cargo fmt --manifest-path crates/adamite-ntfs/fuzz/Cargo.toml --check`.
- **`no_std`** (ubuntu, pinned 1.99 + `thumbv7em-none-eabihf`): `cargo build -p adamite-ntfs --target thumbv7em-none-eabihf`, with and without `--features alloc`.
- **`deny`**: `EmbarkStudios/cargo-deny-action@v2`, `check all`. Allowed licenses: MIT, Apache-2.0, Apache-2.0 WITH LLVM-exception, Unicode-3.0, and only what `cargo deny` additionally demands.
- **`deno`** (ubuntu, `denoland/setup-deno@v2` at the Deno version recorded in `deno.json`):
  - `deno fmt --check`, `deno lint`, `deno check scripts/`;
  - `deno task ci-lint`, which parses `.github/workflows/*.yml` with `jsr:@std/yaml` and fails on syntax errors.
- **`fuzz`** (ubuntu, nightly via `cargo +nightly`): `cargo install cargo-fuzz --locked` (cached), then for each target `cargo fuzz run <t> fuzz/corpus/<t> -- -max_total_time=30 -max_len=262144 -timeout=2 -rss_limit_mb=2048`.

`deno task check` (`scripts/check.ts`) runs the same commands locally, minus Windows and the fuzz job, which is opt-in via `--fuzz`.

With `--fuzz`, every target must have a corpus directory containing a nonempty seed file; a missing or empty corpus fails before the checks run.

### D15. Architecture document corrections

docs/ARCHITECTURE.md is updated as part of this change:

- **§2.2:** field widths and offsets (`total_sectors@0x28`, `mftmirr_lcn@0x38`, `clusters_per_index@0x44`, `serial@0x48`); the exponent encoding for `sectors_per_cluster`; "1024, or 4096 on 4Kn".
- **§2.3, record header:** `lsn@0x08`, `bytes_allocated@0x1C`, `next_attr_id@0x28`; the 3.0 layout (`usa@0x2A`, no `record_no`); the fixup stride is always 512.
- **§2.3, IN_USE check:** it happens before fixup (peek).
- **§2.3, attribute header:** `compression_unit` is a u8 at 0x22 followed by 5 reserved bytes; `compressed_size` is present only in headers of 0x48 bytes or more.
- **§2.3, runlist:** zero-length runs are invalid; the delta base resets per segment.
- **§2.4:**
  - all non-resident size fields, including `allocated_size`, are valid only at `lowest_vcn == 0`;
  - `$ATTRIBUTE_LIST` is decoded only for `$MFT` bootstrap;
  - the root's parent is itself (5:5);
  - record flags 0x4 and 0x8 do not mark user directories.
- **§3.1:** `Patch.base` becomes `FileRef`; `Data.alloc` and `Index.alloc` become `Option<u64>`.
- **§6 step 1:** the fixture strategy.
- **New "Conventions" section:**
  - toolchain pinned to 1.99;
  - snafu errors in every crate (no_std configuration for the parser);
  - tracing from `adamite-core` upward, none in `adamite-ntfs`;
  - Deno TypeScript for all scripts and tasks via `deno task`.

## Risks / Trade-offs

- **ntfs-3g images differ from Windows-made volumes:** no `$UsnJrnl`, different `$Secure`, rarely an attribute-listed `$MFT`. → Synthetic coverage (D11). A later change can add Windows-generated fixtures.
- **Fixture regeneration needs sudo and Linux;** the agent cannot run it unattended. → `deno task fixtures` is a one-shot manual step for the user; tasks mark it explicitly.
- **The Deno generator invokes `sudo` for mount and unmount.** → `--allow-run` is restricted to the listed binaries, the generator only mounts images it created in its own temp dir, and `finally` always unmounts.
- **mkntfs support for 64 KiB clusters with 4K sectors is assumed.** → The recipe asserts the actual geometry and fails loudly. Fallback if unsupported: change `4kn-64k` to a 4 KiB cluster and keep 64K coverage synthetic.
- **Forcing `/frag.bin` to get an attribute list depends on ntfs-3g's allocator.** → Postcondition checks, plus up to 3 retries with a larger hole count in the recipe.
- **Toolchain pin drift:** `rust-toolchain.toml` and `rust-version` must change together. → `scripts/check.ts` fails if they disagree.
- **snafu adds a proc-macro build dependency to a previously dependency-free crate,** making builds slower, especially fuzz builds. → It is accepted for project-wide consistency. Runtime cost is zero because context selectors compile to plain enum construction. The no_std CI job guards the configuration.
- **Lazy iterators report corruption only after earlier items have been yielded.** → Core must exhaust iterators before committing a record; this is documented on `FileRecord::attributes`.
- **Strict lints make offset code verbose.** → All offset arithmetic goes through `bytes.rs` helpers (`read_u16(buf, off) -> Option<u16>`, `range(off, len) -> Option<Range>`).
- **Tolerant SI/FILE_NAME length rules accept slightly malformed values.** → They are bounded by the containing attribute, so there is no safety impact.

## Migration Plan

Not applicable. This is a new crate with no consumers.
