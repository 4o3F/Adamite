# adamite-ntfs

`adamite-ntfs` parses NTFS boot sectors, FILE records, attributes, mapping pairs and typed attribute values from caller-owned bytes. It uses `no_std`, forbids unsafe code, and has snafu as its only runtime dependency. It performs no I/O or logging. Default features are empty; `alloc` enables fallible UTF-16 string conversion and runlist collection.

## Reading records in Phase 1

1. Peek with `RecordHeader::peek` when the caller needs fixed header fields before classification.
2. Call `classify` on the mutable record buffer. It checks size, uniform unused fill, BAAD, FILE signature, header validity and IN_USE before fixup. Base and extension results borrow fixed bytes; other classes leave the buffer unchanged.
3. Exhaust `FileRecord::attributes()` and each required runlist so that late errors are observed.
4. Copy names, references and required scalar fields into caller-owned storage. `FileName` reports its cached sizes, while `Data` reports its own stream values. Name selection, stream accounting and extension-record merging belong to the core.
5. Drop borrowed views before returning the buffer to its pool. Fixup produces a private-constructor `FixedRecord` token consumed by `FileRecord::parse`; protection uses 512-byte strides even on 4Kn volumes.

The callback below copies each name into the caller's chosen storage. Its lifetime bound prevents retaining a borrowed name after the call. The example uses snafu to preserve each parser error category.

```rust
use adamite_ntfs::{
    AttributeError, AttributeType, FileName, FileRef, RecordClass,
    RecordError, RecordHeader, Utf16Le, ValueError, classify,
};
use snafu::Snafu;

#[derive(Debug, Snafu)]
enum ScanError {
    #[snafu(transparent)]
    Record { source: RecordError },
    #[snafu(transparent)]
    Attribute { source: AttributeError },
    #[snafu(transparent)]
    Value { source: ValueError },
}

fn scan_record(
    buffer: &mut [u8],
    mut copy_name: impl for<'view> FnMut(FileRef, Utf16Le<'view>),
) -> Result<(), ScanError> {
    let _header = RecordHeader::peek(buffer)?;
    {
        match classify(buffer) {
            RecordClass::Base(record) | RecordClass::Extension(record) => {
                for attribute in record.attributes() {
                    let attribute = attribute?;
                    if attribute.attribute_type() == AttributeType::FILE_NAME {
                        let name = FileName::from_attribute(attribute)?;
                        copy_name(name.parent(), name.name());
                    }
                }
            }
            RecordClass::Corrupt(error) => return Err(error.into()),
            _ => {}
        }
    }
    buffer.fill(0);
    Ok(())
}
# fn main() -> Result<(), ScanError> {
# let mut buffer = [0u8; 1024];
# scan_record(&mut buffer, |_, _| {})?;
# Ok(())
# }
```

## Accepted format variations

Standard information accepts exactly 48 bytes or at least 72 bytes, ignoring bytes beyond the 72-byte header. File names accept trailing bytes after their declared UTF-16 units. Names preserve NULs, unpaired surrogates and unaligned byte storage; strict and lossy decoders are available without `alloc`. Fixup accepts every USN value. Unknown attribute types, flag bits and reparse tags are preserved. Attribute-list iteration accepts a short all-zero tail of fewer than 26 bytes. Resident value and mapping-pair offsets may be unaligned. Attribute lengths remain 8-aligned, and bounds, fixup integrity, iterator progress and numeric relationships are validated.

Non-resident sizes are exposed only from the first segment, where lowest VCN is zero. Attribute lists decode complete values and segment references for MFT bootstrap; the core resolves those references.

## Errors and ScanStats categories

Parser errors are non-exhaustive snafu enums implementing `core::error::Error`, `Copy`, `Send` and `Sync`, with no backtraces or heap data. Integration tests enforce a maximum size of 24 bytes. The core owns `ScanStats` and maps errors to the following categories.

| Error | Counter categories |
|---|---|
| `GeometryError` | Invalid cluster extent for `InvalidClusterRange`; invalid volume geometry otherwise |
| `FileRefError` | Invalid file reference |
| `FixupError` | Bad fixup for `TrailerMismatch`; invalid fixup metadata otherwise |
| `RecordError` | Nested fixup category for `Fixup`; invalid signature for `InvalidSignature`; corrupt header otherwise |
| `AttributeError` | Corrupt attribute |
| `ValueError` | Type/storage mismatch for `WrongAttributeType` and `WrongStorageForm`; corrupt typed value otherwise |
| `RunlistError` | Out-of-volume extent for `RunOutOfVolume`; allocation failure when enabled; corrupt runlist otherwise |
| `BitmapError` | Invalid bitmap length |
| `AttributeListError` | `Value` follows its nested `ValueError` mapping; corrupt attribute list otherwise |
| `Utf16Error` | Malformed name for `OddLength`; invalid UTF-16 for `UnpairedSurrogate`; allocation failure when enabled |

## Local checks and fixtures

From the repository root, run `deno task check`. It checks formatting, both feature sets with Clippy and tests, documentation, embedded `no_std` builds, dependency policy and Deno scripts. The property tests run 1024 cases per property through this task. `deno test scripts/fixtures/` checks manifest serialization and tool-output parsing.

On Linux, install ntfs-3g, zstd and attr, then run `deno task fixtures` from the repository root. This manual step uses sudo for image mounting, populates and verifies `basic-4k`, `frag-512` and `4kn-64k`, and writes each compressed image together with its independent JSON manifest under `tests/fixtures/ntfs/`. `deno task fixtures --help` prints usage without tool execution. The total compressed budget is 1 MiB. Rust fixture support uses ruzstd and compares decompressed length and frame checksum before bootstrapping the MFT.

## Fuzzing

The separate fuzz package has five targets: `boot_sector`, `file_record`, `attributes`, `runlist` and `typed_values`. From `crates/adamite-ntfs`, run:

```text
cargo install cargo-fuzz --locked
cargo +nightly fuzz build
```

Run `deno task fuzz-corpus` from the repository root to extract deterministic seeds from the committed images, with a 64 KiB budget per target. Re-running this command replaces generated and evolved corpus files with those seeds. Then, from `crates/adamite-ntfs`, run a target with:

```text
cargo +nightly fuzz run file_record fuzz/corpus/file_record/ -- -max_total_time=30 -max_len=262144 -timeout=2 -rss_limit_mb=2048
```

`deno task check --fuzz` requires a nonempty corpus for each target, builds all targets, and runs the five smoke checks. Missing or empty corpora fail the command; run `deno task fuzz-corpus` to restore the seeds.
