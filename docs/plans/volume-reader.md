# Plan: adamite-io volume reader

## Why

- `adamite-core` is sans-io: `scan::Scanner` emits `ScanStep::Need(ReadRequest)` and accepts exact responses through `feed`, but the only callers are core tests and fuzz targets driving in-memory images. Nothing reads an image file or a live volume, so no real `Snapshot` can be produced.
- Each core test that needs a complete scan carries its own ad-hoc `step`/`feed` loop (`tests/fixtures.rs`, `tests/thread_safety.rs`). There is no production driver with defined error, waiting and bounds semantics.
- Query, persistence and CLI work (ARCHITECTURE §4, §6 step 4) need snapshots of real volumes. ARCHITECTURE §5 assigns device access, alignment and host-error propagation to `adamite-io`.

## What Changes

Add the workspace crate `crates/adamite-io` with:

- a `VolumeReader` trait for exact positional reads of known-length sources;
- three readers: borrowed byte slices, image files and Windows raw volumes (`\\.\C:`, unbuffered aligned reads);
- `SyncDriver`, a single-threaded driver that runs a `Scanner` to completion against any reader;
- one error type carrying host, configuration, scan and protocol failures;
- Windows volume context (label, free bytes, scan FILETIME) for `ScanContext`.

Update ARCHITECTURE, core documentation cross-references, the toolchain targets, the local gate and CI for the new crate.

## Scope

In scope: the crate above, its tests, a Windows-target Clippy step in the local gate, a Windows CI raw-volume smoke step, documentation, and a manual-verification example (D11).

Out of scope: parallel parsing, buffer pools, native I/O depth above 1, rate limits and I/O/CPU priority (roadmap step 3); Linux block devices with `O_DIRECT`; VHD attach tests; compressed or container image formats (`.zst`, VHD/VHDX, partitioned disks); queries, persistence, CLI, MCP, services and USN.

Core and parser implementation and tests, fixtures, fuzz targets and corpora remain unchanged. The only core edits are documentation text referring to the "future adamite-io" in `crates/adamite-core/README.md` and the `src/lib.rs` crate doc.

Follow the project's Rust best practices. Production types carry no fields, variants or hooks used only by tests, and no lint suppression (`allow`/`expect(dead_code)`) is added to keep intermediate milestones green. Add no bash/Python scripts; automation stays in Deno.

## Impact

- New: `crates/adamite-io/{Cargo.toml,README.md,src/**,tests/**,examples/scan.rs}`.
- `Cargo.lock` gains `windows` (Windows-only target dependency) and its `windows-core` family (`windows-result`, `windows-strings`, `windows-link`, the `windows-implement`/`windows-interface` proc macros and their `syn` dependencies); `adamite-io` dev-depends on `adamite-fixtures` and `proptest`.
- `rust-toolchain.toml`: `targets = ["thumbv7em-none-eabihf", "x86_64-pc-windows-gnu"]` (D10).
- `scripts/check.ts`, `scripts/check_test.ts`: Windows Clippy step and assertions (D10); CI step assertion (D11). `.github/workflows/ci.yml`: Windows raw-volume smoke step (D11). `deno.json` unchanged.
- `docs/ARCHITECTURE.md` §1, §5, §6, §7; core README and crate-doc text (see Scope).

## Design decisions

1. D1. Layout and public paths. `lib.rs` contains crate docs, lint attributes and module declarations only: public `error`, `reader`, `driver`; private `align` (D7). No root re-exports, no public free functions; each public item has one path.

   | Module                                                  | Public items                                                                                  |
   | ------------------------------------------------------- | --------------------------------------------------------------------------------------------- |
   | `error`                                                 | `error::VolumeError`                                                                          |
   | `reader` (private children `slice`, `image`, `windows`) | `reader::{VolumeReader, SliceReader, ImageReader}`; `reader::RawVolumeReader` on Windows only |
   | `driver`                                                | `driver::SyncDriver`                                                                          |

   `reader/mod.rs` declares its children privately and re-exports their types. This follows the core-layout facade convention (`adamite_core::scan`), not ntfs-layout's no-facade rule: readers are siblings of one concept, and callers should not see the per-source files. `reader/windows/` (`mod.rs`, `ffi.rs`) is `#[cfg(windows)]`.

2. D2. Reader contract.

   ```rust
   pub trait VolumeReader {
       /// Source length in bytes.
       fn len_bytes(&self) -> u64;
       /// Fills `buf` with the bytes at `[offset, offset + buf.len())`.
       fn read_exact_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), VolumeError>;
   }
   ```

   Object-safe, unsealed (integration tests implement fault readers), no `Send` supertrait. `&mut self` lets readers own reusable bounce buffers without interior mutability. Success means `buf` is completely filled; implementations never zero-fill or return partial data. Callers own the destination buffer, so the driver reuses one allocation; the extra copy relative to a lending API is accepted because `feed` copies anyway. A future parallel reader (roadmap step 3) is a separate `&self` trait, not a change to this one.

   Bounds: the driver checks every request against `len_bytes()` before reading (D4). Readers check again defensively and return the same `OutOfBounds` variant; tests assert variant and fields, not which layer raised it.

3. D3. Readers.
   - `SliceReader<'a>::new(bytes: &'a [u8])`: bounds check then `copy_from_slice`.
   - `ImageReader::open(path: &Path) -> Result<Self, VolumeError>`: read-only `File`, length from the opened handle's metadata. Both platforms use one private exact-read loop, `image::ExactRead`, generic over a positional read primitive (`FnMut(u64, &mut [u8]) -> io::Result<usize>`): Unix passes `FileExt::read_at`, Windows `FileExt::seek_read`. The loop advances offset and buffer, retries `Interrupted`, maps a zero-byte read to `ShortRead { offset, expected, actual }` and any other error to `Read`; both report the original request's offset and length, not a partial remainder. A file truncated during the scan surfaces as `ShortRead`. The image is the uncompressed volume starting at byte 0.
   - `RawVolumeReader` (Windows): see D7.

4. D4. Driver.

   ```rust
   impl SyncDriver {
       pub fn new(config: ScanConfig) -> Result<Self, VolumeError>; // validates early
       pub fn config(&self) -> &ScanConfig;
       pub fn run<R: VolumeReader + ?Sized>(&self, reader: &mut R) -> Result<ScanOutput, VolumeError>;
   }
   ```

   `run` creates a fresh `Scanner` from a clone of the config, so a driver is reusable. Loop:
   - `Need(request)`: check `offset + len <= reader.len_bytes()` with checked arithmetic, else `OutOfBounds`; push onto an issue-ordered `VecDeque` (fallible reserve). Do not read yet.
   - `Progress { waiting_for_input: false, .. }`: continue without reading; consecutive CPU progress steps are legal and unbounded.
   - `Progress { waiting_for_input: true, .. }`: serve every queued request in issue order — read into the reused buffer (fallibly grown to the largest request), then `feed` immediately. A waiting step with an empty queue returns `Protocol` (phase and outstanding count). Core only waits when its front slot has no response (`scanner.rs` input status), so this indicates a core state error, never a legitimate state.
   - `Done(output)`: an empty queue is required (`Protocol` otherwise); return the output.
   - Unknown `ScanStep` variants (`#[non_exhaustive]`): `Protocol`.

   Waiting rather than "queue full" triggers service, because boot and discovery wait below capacity. Native read depth is 1; `max_outstanding > 1` only batches requests. The step handling lives in a private type (e.g. `driver::Pending`) whose methods take a `ScanStep` and a reader, so unit tests can feed synthetic steps. The driver emits a `tracing` debug event per served batch and no other logging.

5. D5. Errors. `error::VolumeError` is one `#[non_exhaustive]` snafu enum with `visibility(pub(crate))`, consistent with core. Variants (no `cfg` on any variant, so matching is identical across targets): `Open { path, source: io::Error }`, `AccessDenied { path, source: io::Error }`, `InvalidDrive { letter: char }`, `Query { operation: &'static str, source: io::Error }`, `UnsupportedAlignment { logical, physical }`, `UnalignedVolume { volume_len, logical }`, `Read { offset, len, source: io::Error }`, `ShortRead { offset, expected, actual }`, `OutOfBounds { offset, len, volume_len }`, `Overflow { operation: &'static str }`, `Allocation { source: TryReserveError }`, `Config { source: ConfigError }`, `Scan { source: ScanError }`, `Feed { request: ReadRequest, source: FeedError }`, `Protocol { reason: &'static str, phase: Option<ScanPhase>, outstanding: usize }`. Core errors are wrapped, never stringified. `AccessDenied` is mapped from `ERROR_ACCESS_DENIED` on open; its message does not claim the process is unelevated. `VolumeError: Send + Sync`. Every public fallible function documents `# Errors`.

6. D6. Lints and unsafe boundary. The crate keeps the workspace `unsafe_code = "deny"` and adopts core's panic-free Clippy denies. `lib.rs` adds `#![cfg_attr(not(windows), forbid(unsafe_code))]`, so non-Windows builds are fully forbidden. `reader/windows/ffi.rs` is the only module with `#![allow(unsafe_code)]`, plus `#![deny(clippy::undocumented_unsafe_blocks, clippy::multiple_unsafe_ops_per_block)]`; every block has a specific `// SAFETY:` comment.

   `ffi.rs` exposes only safe, typed, `pub(super)` operations owned by two private types: `VolumeHandle<'a>(&'a File)` with methods `allow_extended_dasd`, `length` and `access_alignment`; and `VolumeRoot` (an `HSTRING` holding `X:\`, passed as `PCWSTR`) with `label` and `free_bytes`. No generic IOCTL helper takes caller-chosen output types. The device is opened and read with safe std APIs (`OpenOptionsExt::{access_mode, share_mode, custom_flags}`, `FileExt::seek_read`); unsafe is limited to `DeviceIoControl`, `GetVolumeInformationW` and `GetDiskFreeSpaceExW`.

   Dependency: `[target.'cfg(windows)'.dependencies] windows = { version = "0.62", features = ["Win32_Foundation", "Win32_Storage_FileSystem", "Win32_System_Ioctl", "Win32_System_IO"] }` (default `std` feature kept). The `windows` crate is preferred over `windows-sys` for its typed wrappers: `HANDLE`/`PCWSTR`/`HSTRING`, slice parameters for `GetVolumeInformationW`, `Option` pointers, and `windows::core::Result` returns, which convert into `io::Error` through the crate's `From` impl so `VolumeError` keeps `io::Error` sources. The Win32 functions remain `unsafe fn`, so the D6 boundary is unchanged. Its crates are MIT OR Apache-2.0 and the proc-macro dependencies (`syn` family, `unicode-ident`) fall within the existing `deny.toml` allow-list; `cargo deny check` confirms it in M4. Alternative rejected: a separate Windows platform crate (stronger root-level `forbid`, but an extra crate and cross-crate types for three calls).

7. D7. Windows raw volume.
   - `RawVolumeReader::open_drive(letter: char)`: ASCII letters only (`InvalidDrive` otherwise); device path `\\.\X:` (no trailing backslash), root path `X:\` for volume queries. Open with `GENERIC_READ`, `FILE_SHARE_READ | FILE_SHARE_WRITE`, `OPEN_EXISTING`, `FILE_FLAG_NO_BUFFERING`, synchronous handle. Then `FSCTL_ALLOW_EXTENDED_DASD_IO` (`FILE_ANY_ACCESS`) so trailing sectors are readable.
   - Length: `IOCTL_DISK_GET_LENGTH_INFO` (positive, checked conversion). Alignment: `IOCTL_STORAGE_QUERY_PROPERTY` / `StorageAccessAlignmentProperty`; validate returned size/version, non-zero powers of two and `physical >= logical`; otherwise `UnsupportedAlignment` (no guessed 4096 fallback). `BytesOffsetForSectorAlignment` is not used. Open fails with `UnalignedVolume` unless `volume_len % logical == 0`.
   - Private `align::Span` (pure, cross-platform): for request `[o, o + n)` and logical sector `L`, native range `[floor(o, L), ceil(o + n, L))` plus the request's slice offset inside it. The span is never truncated; its end must not exceed the (sector-multiple) volume length, otherwise `OutOfBounds`. Checked arithmetic, `Overflow` on failure.
   - Private `align::AlignedBuf`: safe over-allocation (`try_reserve_exact(len + align - 1)`), padding from `as_ptr().align_offset(physical)`, aligned subslice; reused and grown fallibly across reads; no `unsafe`, no `Layout`.
   - Each request performs one native `seek_read` of the whole span; anything short of the span returns `ShortRead` (continuing from an advanced address could break alignment). Transfers larger than the adapter maximum rely on the storage class driver's splitting; a host failure surfaces as `Read`. The 512-byte boot request becomes a 4096-byte native read on 4Kn devices, sliced to 512 bytes.
   - `RawVolumeReader::context(&self) -> Result<ScanContext, VolumeError>`: label from `GetVolumeInformationW` (empty label → `None`), total free bytes from `GetDiskFreeSpaceExW` (`lpTotalNumberOfFreeBytes`, not quota-limited available bytes), and the current FILETIME (D8). Host query failures propagate as `Query`; callers set `config.context = reader.context()?` before creating the driver. The driver never collects context.
   - Live volumes may change during a scan. Torn ordinary records are counted as corrupt by core; a torn record 0 or attribute list during bootstrap/discovery returns a fatal `ScanError`. The driver keeps the error chain and stops; nothing is retried or reopened. Locked BitLocker volumes and removed devices surface as host errors.

8. D8. Scan timestamp. Use the `nt-time` crate (`[target.'cfg(windows)'.dependencies] nt-time = { version = "0.15", default-features = false, features = ["std"] }`, Apache-2.0 OR MIT) rather than hand-written epoch arithmetic: `nt_time::FileTime::try_from(SystemTime::now())` then `to_raw()`. Its `FileTime::now()` panics on out-of-range clocks, so the fallible conversion is used and maps to `Overflow`. No FFI and no adamite-io time module; conversion correctness is the library's and is covered by its own tests.

9. D9. Tests.
   - `align` unit tests and proptests run on Linux: logical/physical sizes {512, 4096, 65536}; the span covers the request, is sector-aligned, never exceeds the volume and has an exact slice; requests whose span would pass a sector-multiple volume end and volume lengths that are not sector multiples are rejected; buffer padding yields an aligned slice inside the allocation; overflow near `u64::MAX`. The elevated context test checks the FILETIME is later than the Unix epoch.
   - `image::ExactRead` unit tests with scripted primitives: `Interrupted → partial → rest`, `partial → 0` (`ShortRead`), `partial → error` (`Read`); assert the offsets actually passed to the primitive and the original range in the error.
   - Driver step-handler unit tests (synthetic `ScanStep`s and a recording reader): several `Need`s only enqueue; `Progress(false)` reads nothing; `Progress(true)` reads and feeds in issue order as one batch; waiting below capacity is served; waiting with an empty queue, `Done` with a non-empty queue and the overflow path return `Protocol`/`Overflow`.
   - `tests/equivalence.rs`: for `basic-4k`, `frag-512`, `4kn-64k`, an independent minimal in-memory baseline loop (respond to each `Need` immediately, as `core/tests/thread_safety.rs` does) versus `SyncDriver` over `SliceReader` and over `ImageReader` (fixture image written to `env!("CARGO_TARGET_TMPDIR")` under a per-test name; no `tempfile` dependency). Configs: default; cap 4 × 512-byte chunks; cap 1 × 4096; cap 2 × 16 MiB. Same fixed `ScanContext`; `assert_eq!` on the complete `ScanOutput`.
   - `tests/faults.rs`: a test-local fault reader returning `ShortRead` or `Read` on read N (N = 1 boot sector, N = 2 record 0, N = last read of a full scan); missing path (`Open`); `basic-4k` truncated to its 512-byte boot sector (`OutOfBounds` on the record-zero request, whose `volume_len` equals the truncated length; the fixture scan only touches its first half, so a half-length image still succeeds); image truncated after open (`ShortRead`); invalid config (`Config`); 512 zero bytes (`Scan` wrapping `ScanError::Geometry`). Assert variant, fields and `source()` chains; no test relies on panics.
   - Variant coverage: Linux and Windows integration tests — `Open`, `Read`, `ShortRead`, `OutOfBounds`, `Config`, `Scan`; private unit tests — `Protocol`, `Overflow`, `UnalignedVolume` (span/validation logic); non-ignored Windows test — `InvalidDrive` (`open_drive('1')`); `AccessDenied`, `Query` and `UnsupportedAlignment` depend on the host environment and are covered by code review, not forced in tests; `Allocation` and `Feed` are unreachable with exact buffers and are not exercised.
   - `tests/thread_safety.rs`: created in M1 with `SliceReader<'static>`, `SyncDriver` (`Send`) and `VolumeError` (`Send + Sync`) plus a complete scan on a spawned thread; M2 adds `ImageReader`, M4 adds `RawVolumeReader` (`#[cfg(windows)]`).
   - `tests/raw_volume.rs` (`#![cfg(windows)]`): non-ignored `InvalidDrive` test; tests needing elevation are `#[ignore = "requires an elevated Windows session"]`: open `C:`, check length is a sector multiple and alignment is valid, read the boot sector and match the `NTFS` OEM id, read the last logical sector, run a full default-config scan, assert a non-empty snapshot, `records_in_use > 0`, and a context with FILETIME and free bytes. No exact counts (live volume).
   - Core's own test drivers are not migrated: they exercise arrival orders, and migration would create a core → io dev-dependency cycle.

10. D10. Toolchain and local gate. `rust-toolchain.toml` gains `targets = ["thumbv7em-none-eabihf", "x86_64-pc-windows-gnu"]`, so rustup installs both automatically (the gate may not run `rustup`/`install`; `check.ts` reads only `channel`). `scripts/check.ts` inserts a "Windows Clippy" step after "Clippy": `cargo clippy -p adamite-io --target x86_64-pc-windows-gnu --all-targets --locked -- -D warnings` (type-check only, no MinGW linker needed). `scripts/check_test.ts` extends the normal-checks test with an `equal` on `steps.filter((step) => step.args.includes("x86_64-pc-windows-gnu"))`. CI does not mirror this step: native workspace Clippy on `windows-latest` covers it.

11. D11. CI and manual verification.

- `.github/workflows/ci.yml`, `check` job: one step `run: cargo test -p adamite-io --test raw_volume --locked -- --ignored` with `if: runner.os == 'Windows'` (hosted Windows runners run elevated). The existing `cargo test --workspace` on Windows runs every non-ignored test. `scripts/check_test.ts`: add `if?: string` to `WorkflowStep` and assert `jobs.check.steps` contains exactly one step with that `run` and `if`.
- `examples/scan.rs`: `cargo run -p adamite-io --example scan -- <image-path>` or `-- --drive C`. Manual argument parsing, no new dependencies; prints `ScanStats`, volume info and the root's largest children, or the full error chain with exit status 1. On non-Windows, `--drive` prints `raw volumes require Windows` and exits with status 2; the example compiles on all targets. Used for the manual Windows/WizTree comparison noted in ARCHITECTURE §6.

12. D12. Documentation. ARCHITECTURE: diagram and workspace table (adamite-io implemented), §1 key decisions (adapter owns alignment and context; unsafe confined to one Windows FFI module), §5 intro and §5.4 (synchronous NO_BUFFERING implemented; pools still planned), §6 (step 3 done, next is roadmap parallelism or `add-core-query`), §7 (core forbids unsafe; adamite-io's single FFI module; the adapter reads the clock). New `crates/adamite-io/README.md` (module tree, driving example, errors, elevation, toolchain targets). A runnable `lib.rs` doctest drives `SyncDriver` over `SliceReader::new(&[0u8; 512])` and matches `VolumeError::Scan { source: ScanError::Geometry { .. } }`; successful scans are covered by `tests/equivalence.rs`, keeping the unpublished `adamite-fixtures` out of public docs. Historical plans remain unchanged.

## Tasks

Each milestone is one bounded Codex run (< 40 minutes) and one reviewed commit, ending with `deno task check` green.

### M1 — Crate skeleton, slice reader and driver

- [x] Add the workspace member with D1 layout (no `align`/`filetime`/Windows modules yet), D5 errors, D6 lints, `reader::{VolumeReader, SliceReader}` and `driver::SyncDriver` per D2/D4, the D12 doctest and the driver step-handler unit tests (D9).
- [x] Add `tests/equivalence.rs` (slice half), `tests/faults.rs` (fault reader, config and scan cases) and `tests/thread_safety.rs` (M1 types). Verify `cargo test -p adamite-io --locked`; finish with `deno task check` green.

### M2 — Image file reader

- [x] Add `reader::ImageReader` and private `image::ExactRead` per D3 with its scripted unit tests. Extend equivalence to image files in `CARGO_TARGET_TMPDIR`, faults to missing, truncated and truncated-after-open images, and thread safety to `ImageReader`. Finish with `deno task check` green.

### M3 — Windows lint gate

- [x] Add the D10 toolchain targets, "Windows Clippy" step and `check_test.ts` assertion. Finish with `deno task check` green.

### M4 — Windows raw volume reader

- [x] Add private `align::{Span, AlignedBuf}` per D7, declared `#[cfg(any(windows, test))]` so it arrives together with its production consumer and its D9 unit tests and proptests run on Linux. Add the Windows target dependencies (`windows`, `nt-time` per D8), `reader/windows/{mod,ffi}.rs` and `RawVolumeReader` per D6/D7 (open, DASD, length, alignment, aligned reads, `context()`), `tests/raw_volume.rs` per D9 and the `RawVolumeReader` thread-safety check.
- [x] Verify the Windows Clippy gate step, `cargo deny check`, and that Linux builds compile the module out; finish with `deno task check` green. Windows execution is verified by CI after the commit (M5 adds the smoke step).

### M5 — CI smoke, example and documentation

- [ ] Add the D11 CI step with its `check_test.ts` assertion and `examples/scan.rs`. Update documentation per D12 and tick the acceptance criteria.
- [ ] Finish with `deno task check` green; after the commit, confirm both CI `check` matrix jobs, including the elevated raw-volume smoke step, pass.

## Acceptance criteria

- [ ] Inspect `rg -n '^(#\[cfg[^\]]*\]\s*)?(pub )?mod\b' crates/adamite-io/src/lib.rs` against D1. `rg -n '^pub use\b' crates/adamite-io/src/lib.rs`, `rg -n '^pub(\([^)]*\))?\s+(const\s+)?fn\b' crates/adamite-io/src` and `rg -n '^\s*pub(\([^)]*\))?\s+mod\b' crates/adamite-io/src/reader/mod.rs` have no matches.
- [ ] `rg -n '\bunsafe\s*(\{|fn\b|impl\b|extern\b|trait\b)' crates/adamite-io --glob '!src/reader/windows/ffi.rs'` has no matches; `rg -n 'allow\(unsafe_code\)' crates/adamite-io` has exactly one match, in `ffi.rs`; Windows Clippy enforces `undocumented_unsafe_blocks`. `rg -n 'allow\(dead_code\)|expect\(dead_code' crates/adamite-io` has no matches.
- [ ] For all three fixtures and all D9 configs, `SyncDriver` over `SliceReader` and `ImageReader` produces a `ScanOutput` equal to the independent baseline; driver unit tests show multi-request batches served in issue order.
- [ ] The D9 variant-coverage list holds, with offsets, lengths and source chains asserted.
- [ ] `deno task check` passes, including the Windows Clippy step and `cargo deny check`; `deno task check --fuzz` passes with corpora unchanged.
- [ ] CI `check (windows-latest)` passes the ignored raw-volume smoke tests, and `check (ubuntu-latest)` passes.
- [ ] Manual: after `zstd -d tests/fixtures/ntfs/basic-4k.img.zst -o target/basic-4k.img`, `cargo run -p adamite-io --example scan -- target/basic-4k.img` prints statistics. ARCHITECTURE and READMEs match the final code; core/parser implementation and tests, fixtures and fuzz corpora are unchanged (core diff is documentation text only).
