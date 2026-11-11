# adamite-io

`adamite-io` reads NTFS volume bytes and drives `adamite-core` scans to an immutable snapshot and statistics. It provides borrowed slices, read-only image files and Windows raw volumes through `reader::VolumeReader`, with exact positional reads into caller-owned buffers. `driver::SyncDriver` runs on the calling thread and serves queued requests in issue order when core waits for input.

The public API lives under `reader::`, `driver::`, `limits::`, `error::`, `snapshot::`, `snapshot_dir::`, `elevation::`, `status::` and Windows-only `volume::`; the crate root exports modules only. Reader implementation modules are private and their types are exported directly under `reader::`.

```text
src/
  lib.rs            crate docs, attributes and module declarations
  error.rs          VolumeError with host and core error sources
  driver.rs         SyncDriver and private request batching
  limits.rs         ScanLimits and shared ScanProfile defaults
  snapshot_dir.rs   platform snapshot directory and saved-file listings
  volume/ffi.rs     Windows NTFS drive discovery boundary
  pacer.rs          private token bucket and clock abstraction
  reader/
    mod.rs          VolumeReader and reader type exports
    slice.rs        SliceReader borrowing an immutable byte slice
    image.rs        ImageReader for uncompressed volume images
    windows/        RawVolumeReader (Windows only)
      mod.rs        device opening, aligned reads and scan context
      ffi.rs        private Windows API boundary
  align.rs          private checked sector spans and reusable aligned storage
```

## Driving a scan

```rust
use adamite_io::{driver::SyncDriver, limits::ScanProfile, reader::ImageReader};
use std::{error::Error, path::Path};

fn main() -> Result<(), Box<dyn Error>> {
    let mut reader = ImageReader::open(Path::new("target/basic-4k.img"))?;
    let driver = SyncDriver::with_limits(ScanProfile::Balanced.limits(), reader.context()?)?;
    let output = driver.run(&mut reader)?;
    println!("{:?}", output.stats);
    println!("{:?}", output.snapshot.volume());
    Ok(())
}
```

Sources start at volume byte zero. Decompress fixture images before opening them; partitioned disks and VHD/VHDX containers require extraction of the volume image first. `ImageReader` caches the opened file's length, retries interrupted reads and completes partial reads; truncation after opening returns `ShortRead`.

`ScanLimits::default()` selects Balanced: 256 MiB/s, one outstanding request, 1 MiB chunks and a worker ceiling of one. Background selects 64 MiB/s with 1 MiB chunks; Max disables pacing and uses 4 MiB chunks. Every positive `threads` ceiling is accepted; the synchronous driver uses one thread. Future parallel work must honour that ceiling. `ScanLimits::config(context)` produces core configuration and validates the chunk and outstanding-request bounds through core. `SyncDriver::new(ScanConfig)` keeps the uncapped configuration API. The driver validates configuration at construction and starts a fresh scanner on each `run`. Increasing `max_outstanding` batches logical requests; native reads remain synchronous with depth 1. The driver reuses its response buffer within a run. The caller supplies optional `ScanContext` before constructing the driver. `ImageReader::context()` supplies the current FILETIME scan timestamp, with `label` and `free_bytes` set to `None`; it returns `VolumeError::Overflow` if the clock cannot be converted. On Windows, `RawVolumeReader::context()` also supplies the volume label and free bytes.

`SyncDriver::run_observed(&mut reader, observer)` uses the same driving loop as `run` and calls the observer after every scanner progress step and after each completed read batch. `driver::DriverProgress` contains the latest `ScanProgress` and cumulative `bytes_read`, including bootstrap and discovery reads. The byte count measures successful logical reads through `VolumeReader`. A batch report retains the latest scanner report; its input-wait flag describes the preceding scanner step. The synchronous `FnMut` observer cannot fail or cancel the scan. Keep callbacks cheap; display throttling belongs to the caller. `run` supplies a no-op observer.

The token bucket starts empty and holds one chunk. When the budget R exceeds the chunk C, refill is R − C bytes per second, so any interval t admits at most B + (R − B)t bytes. This reserves the burst allowance and keeps every one-second window within the profile budget. Rates at or below one chunk per second refill at R, with a one-chunk burst allowance. Pacing counts requested logical bytes; Windows sector alignment can expand native reads. Each wait emits `rate_limit_wait` with bytes and actual waited nanoseconds. Time and sleeping belong to I/O; injected-clock tests perform no real sleeps.

## Errors

`error::VolumeError` is a non-exhaustive snafu enum that preserves host and core sources through `std::error::Error::source()`.

- `Open`, `AccessDenied` and `Query` describe host open and metadata-query failures; `InvalidDrive` rejects drive identifiers other than a single ASCII letter.
- `UnsupportedAlignment` and `UnalignedVolume` reject invalid sector sizes and lengths with a partial final sector.
- `Read`, `ShortRead` and `OutOfBounds` retain the requested offset and length; successful reads fill the complete destination buffer.
- `Overflow` and `Allocation` report checked arithmetic and storage reservation failures.
- `Config`, `Scan` and `Feed` wrap core errors. `Protocol` reports inconsistent scanner steps and pending-request state.

## Windows raw volumes

Run raw-volume scans in an elevated Windows session. `reader::RawVolumeReader::open_drive('C')` opens `\\.\C:` read-only with read/write sharing and `FILE_FLAG_NO_BUFFERING`, then enables extended direct access storage device (DASD) I/O through `FSCTL_ALLOW_EXTENDED_DASD_IO` so trailing sectors are readable. Open failures preserve the host error, including `AccessDenied`.

The reader queries volume length and logical/physical sector sizes. It requires nonzero power-of-two sector sizes, physical sectors at least as large as logical sectors, and a volume length divisible by the logical sector size. Each nonempty request expands outward to complete logical sectors, uses a reusable buffer aligned to the physical sector size, and performs one native synchronous read. Only the requested bytes are copied to the caller. A 512-byte boot request on a 4Kn volume therefore reads 4096 native bytes. A short native read returns `ShortRead`.

`context()` queries the label with `GetVolumeInformationW` and total free bytes with `GetDiskFreeSpaceExW`, using the total-free output rather than quota-limited available bytes. It converts the current clock to FILETIME through the fallible `nt-time` conversion. An empty label becomes `None`; query and timestamp failures propagate to the caller.

Live volumes can change during scanning. Core counts torn ordinary records as corrupt; fatal bootstrap/discovery errors stop the driver with the source chain intact. Host errors from locked BitLocker volumes or removed devices propagate without reopening or retrying the scan.

## Unsafe policy

The crate denies unsafe code and forbids it on non-Windows targets. Private `reader/windows/ffi.rs` wraps raw-volume queries, Windows-only `volume/ffi.rs` wraps drive discovery, and `elevation/ffi.rs` wraps process-token queries, shell launching, process waiting and handle cleanup. These boundaries use the `windows` crate behind safe, typed operations and deny undocumented unsafe blocks and multiple unsafe operations per block. Device opening, positional reads and aligned-buffer storage use safe Rust.

## Volume discovery and snapshot directory

On Windows, `volume::Volume::list()` returns fixed and removable NTFS volumes sorted by drive letter. Each volume exposes its uppercase `letter()`, `label()` and total `capacity()` in bytes. Discovery uses `GetLogicalDriveStringsW`, `GetDriveTypeW`, `GetVolumeInformationW` and `GetDiskFreeSpaceExW`. Drives whose filesystem, label or capacity cannot be queried are skipped, including empty card readers; enumeration failures return `VolumeListError`. The Windows CI test checks that the system volume `C` appears.

`snapshot_dir::SnapshotDir::default_path()` uses `etcetera::choose_base_strategy()` to locate the platform data directory and appends `adamite/snapshots`. `SnapshotDir::new(path)` selects a directory without creating it; call `create()` before the first save. `path()` borrows the directory, and `drive_snapshot('c')` returns its `C.adm` path.

`entries()` lists direct files with an ASCII case-insensitive `.adm` extension, follows symbolic links, ignores subdirectories and sorts by filename. A missing directory gives an empty list. Each `SnapshotEntry` exposes `path()`, `file_stem()`, `byte_len()` and `modified()`; `error()` retains individual metadata or enumeration failures so other files remain available. An enumeration failure without a filename has no path or stem and sorts last. Failure to open the directory itself returns `SnapshotDirError`.

## Elevation and scan status

On Windows, `elevation::ElevatedProcess::is_current()` queries the current process token. `launch(args)` runs the current executable through the UAC prompt with `runas` and a hidden window. `try_wait()` polls for an exit code, `wait()` blocks for it, and dropping the process closes its handle. A cancelled prompt returns `ElevationError::Declined`. Portable `elevation::CommandLine` quotes every argument and doubles trailing backslashes; quotes and NUL characters are rejected.

`status::ScanStatusWriter::create(path)` exclusively creates an append-only JSON-lines file. `write(&ScanStatus)` appends and flushes one line. `progress(&DriverProgress, Instant)` writes the first report, every phase change and otherwise at most one report every 500 ms. `ScanStatus::from_progress` maps core phases to stable snake_case names, and `driver_progress()` maps a progress line with a known phase back to `DriverProgress`. `ScanStatusReader::open(path)` and `poll()` return new complete lines while retaining an incomplete trailing line; a malformed complete line returns `StatusError::Json`. The caller writes the final success or failure outcome and manages file cleanup.

```json
{"progress":{"phase":"stream","records":10,"planned":20,"bytes":4096}}
{"done":{"saved":"scan.adm","bytes":1024}}
```

A failure outcome is `{"failed":{"message":"scan failed","exit_code":1}}`; unknown record totals use `"planned":null`.

## Local checks and fixtures

From the repository root:

```text
deno task check
cargo test -p adamite-io --locked
cargo clippy -p adamite-io --target x86_64-pc-windows-gnu --all-targets --locked -- -D warnings
```

`rust-toolchain.toml` installs the `x86_64-pc-windows-gnu` target. `deno task check` includes Windows Clippy alongside native workspace checks; cross-target Clippy requires no MinGW linker. Unit tests cover alignment, exact image reads and driver protocol handling. Integration tests compare slice/image scans with an independent baseline on all three fixtures, inject read failures, check source chains and verify thread bounds.

Run the ignored raw-volume tests in an elevated Windows session:

```text
cargo test -p adamite-io --test raw_volume -- --ignored
```

CI runs these ignored tests only on Windows, after the normal workspace tests. For command-line image and raw-volume scans, use [`adamite-cli`](../adamite-cli/README.md).

Parallel parsing, buffer pools, native I/O depth above 1 and Linux block-device access remain future work. Adamite enforces its resource limits without operating-system priority APIs.

## Snapshot files

`snapshot::SnapshotWriter::write(&output, &mut file)` writes a new empty seekable file in chunks of at most 1 MiB, then seeks back to patch the CRC and leaves the cursor at the file end. The caller creates, synchronizes, closes and publishes the file. `snapshot::SnapshotReader::read(&mut file)` uses a fallibly allocated 1 MiB file buffer: the first sequential pass verifies the header, table and CRC, and the second decodes directly into final columns before snapshot validation. Keep the file unchanged across both passes. `SnapshotIoError` preserves codec, host I/O, buffer-allocation and host-length failures. CLI saves and benchmark persistence share these adapters.
