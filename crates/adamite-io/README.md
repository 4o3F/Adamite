# adamite-io

`adamite-io` reads NTFS volume bytes and drives `adamite-core` scans to an immutable snapshot and statistics. It provides borrowed slices, read-only image files and Windows raw volumes through `reader::VolumeReader`, with exact positional reads into caller-owned buffers. `driver::SyncDriver` runs on the calling thread and serves queued requests in issue order when core waits for input.

The public API lives under `reader::`, `driver::` and `error::`; the crate root exports modules only. Reader implementation modules are private and their types are exported directly under `reader::`.

```text
src/
  lib.rs            crate docs, attributes and module declarations
  error.rs          VolumeError with host and core error sources
  driver.rs         SyncDriver and private request batching
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
use adamite_core::scan::ScanConfig;
use adamite_io::{driver::SyncDriver, reader::ImageReader};
use std::{error::Error, path::Path};

fn main() -> Result<(), Box<dyn Error>> {
    let mut reader = ImageReader::open(Path::new("target/basic-4k.img"))?;
    let driver = SyncDriver::new(ScanConfig {
        context: reader.context()?,
        ..ScanConfig::default()
    })?;
    let output = driver.run(&mut reader)?;
    println!("{:?}", output.stats);
    println!("{:?}", output.snapshot.volume());
    Ok(())
}
```

Sources start at volume byte zero. Decompress fixture images before opening them; partitioned disks and VHD/VHDX containers require extraction of the volume image first. `ImageReader` caches the opened file's length, retries interrupted reads and completes partial reads; truncation after opening returns `ShortRead`.

`ScanConfig::default()` selects one outstanding request and 1 MiB chunks. The driver validates configuration at construction and starts a fresh scanner on each `run`. Increasing `max_outstanding` batches logical requests; native reads remain synchronous with depth 1. The driver reuses its response buffer within a run. The caller supplies optional `ScanContext` before constructing the driver. `ImageReader::context()` supplies the current FILETIME scan timestamp, with `label` and `free_bytes` set to `None`; it returns `VolumeError::Overflow` if the clock cannot be converted. On Windows, `RawVolumeReader::context()` also supplies the volume label and free bytes.

The [scan example](examples/scan.rs) prints `ScanStats`, `VolumeInfo` and up to ten root children in descending subtree allocation order. Child output includes names, allocated bytes and logical bytes. Errors print their complete source chain.

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

The crate denies unsafe code and forbids it on non-Windows targets. The sole exception is private `reader/windows/ffi.rs`, which wraps `DeviceIoControl`, `GetVolumeInformationW` and `GetDiskFreeSpaceExW` from the `windows` crate behind safe, typed operations. That module denies undocumented unsafe blocks and multiple unsafe operations per block. Device opening, positional reads and aligned-buffer storage use safe Rust.

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
cargo run -p adamite-io --example scan -- --drive C
```

CI runs these ignored tests only on Windows, after the normal workspace tests. On Linux, the example's `--drive` mode prints `raw volumes require Windows` and exits with status 2.

For a manual image scan with zstd installed:

```text
zstd -d tests/fixtures/ntfs/basic-4k.img.zst -o target/basic-4k.img
cargo run -p adamite-io --example scan -- target/basic-4k.img
```

Parallel parsing, buffer pools, native I/O depth above 1, rate limits, priority controls and Linux block-device access remain future work.
