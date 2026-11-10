# adamite-ntfs

`adamite-ntfs` parses NTFS boot sectors, FILE records, attributes, mapping pairs and typed attribute values from caller-owned bytes. It uses `no_std`, forbids unsafe code, and has snafu as its only runtime dependency. It performs no I/O or logging. It has no Cargo features.

## Modules

Each public item has one path under its owning module:

| Module | Contents and entry points |
|---|---|
| `boot` | `VolumeGeometry::from_boot_sector` and `BootError` |
| `record` | `RecordHeader::peek`, `FileRecord::parse` and `RecordClass::classify`; `reference` contains `FileRef`, `system` contains `u64` record numbers such as `MFT` and `ROOT`, and `fixup` exposes `FixupError` |
| `attribute` | Attribute types, flags and header parsing; `iter::AttributeIter`, `non_resident::{NonResident, StreamSizes}` and `runlist::{Runlist, Run, VcnRange}` |
| `value` | `ValueError` and the `standard_information`, `file_name`, `attribute_list`, `data`, `index_allocation`, `reparse_point` and `bitmap` decoders |
| `utf16` | Borrowed `Utf16Le`, unit iteration, strict/lossy character decoding and `Utf16Error` |

Record parsing owns in-place update-sequence fixup. Typed decoders use their associated constructors, such as `value::file_name::FileName::from_attribute`, while `Attribute` retains its resident-value and non-resident descriptor accessors.

## Reading records

1. Peek with `RecordHeader::peek` when the caller needs fixed header fields before classification.
2. Call `RecordClass::classify` on the mutable record buffer. It checks size, uniform unused fill, BAAD, FILE signature, header validity and IN_USE before fixup. Base and extension results borrow fixed bytes; other classes leave the buffer unchanged.
3. Exhaust `FileRecord::attributes()` and each required runlist so that late errors are observed.
4. Copy names, references and required scalar fields into caller-owned storage. `FileName` reports its cached sizes, while `Data` reports its own stream values. Name selection, stream accounting and extension-record merging belong to the core.
5. Drop borrowed views before returning the buffer to its pool. For direct parsing, `FileRecord::parse(&mut buffer)` validates the header and applies fixup, preserving the buffer on every error. Protection uses 512-byte strides even on 4Kn volumes.

Decode volume geometry with `VolumeGeometry::from_boot_sector(&bytes)` before locating records.

The callback below copies each name into the caller's chosen storage. Its lifetime bound prevents retaining a borrowed name after the call. The example uses snafu to preserve each parser error category.

```rust
use adamite_ntfs::{
    attribute::{AttributeError, AttributeType},
    record::{RecordClass, RecordError, RecordHeader, reference::FileRef},
    utf16::Utf16Le,
    value::{ValueError, file_name::FileName},
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
        match RecordClass::classify(buffer) {
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

Standard information accepts exactly 48 bytes or at least 72 bytes, ignoring bytes beyond the 72-byte header. File names accept trailing bytes after their declared UTF-16 units. Names preserve NULs, unpaired surrogates and unaligned byte storage; strict and lossy decoders are available. Fixup accepts every USN value. Unknown attribute types, flag bits and reparse tags are preserved. Attribute-list iteration accepts a short all-zero tail of fewer than 26 bytes. Resident value and mapping-pair offsets may be unaligned. Attribute lengths remain 8-aligned, and bounds, fixup integrity, iterator progress and numeric relationships are validated.

Non-resident sizes are exposed only from the first segment, where lowest VCN is zero. Attribute lists decode complete values and segment references for MFT bootstrap; the core resolves those references.

## Errors

Parser errors are non-exhaustive snafu enums implementing `core::error::Error`, `Copy`, `Send` and `Sync`, with no backtraces or heap data. Integration tests enforce a maximum size of 24 bytes.

| Error path | Validation failures |
|---|---|
| `boot::BootError` | Boot identification, sector/cluster/record sizes, volume bounds and requested cluster ranges |
| `record::reference::FileRefError` | Record number outside the 48-bit reference field |
| `record::fixup::FixupError` | Update-sequence metadata or a protected-trailer mismatch |
| `record::RecordError` | Record size, signature or header; `Fixup { source }` preserves the nested repair error |
| `attribute::AttributeError` | Attribute headers, regions, lengths, names or non-resident descriptors |
| `value::ValueError` | Wrong attribute type/storage form or invalid typed payload |
| `attribute::runlist::RunlistError` | Mapping-pair encoding, VCN coverage or physical extent bounds |
| `value::bitmap::BitmapError` | Logical bit count exceeding the borrowed byte capacity |
| `value::attribute_list::AttributeListError` | Attribute-list size, entry bounds or segment fields; `Value { source }` preserves typed validation errors |
| `utf16::Utf16Error` | Odd byte length or an unpaired surrogate during strict decoding |

`FileRecord::parse` validates the header before applying fixup and leaves the input unchanged on every error. Attribute, attribute-list and runlist iterators can report corruption after yielding valid entries; exhaust them before accepting the complete structure.

## Local checks and fixtures

From the repository root, run `deno task check`. It checks formatting, Clippy, tests, documentation, an embedded `no_std` build, dependency policy and Deno scripts. The property tests run 1024 cases per property through this task. `deno test --allow-read=scripts/fixtures/testdata scripts/fixtures/` checks manifest serialization and tool-output parsing.

On Linux, install ntfs-3g, zstd and attr, then run `deno task fixtures` from the repository root. This manual step uses sudo for image mounting, populates and verifies `basic-4k`, `frag-512` and `4kn-64k`, and writes each compressed image together with its independent JSON manifest under `tests/fixtures/ntfs/`. `deno task fixtures --help` prints usage without tool execution. The total compressed budget is 1 MiB. Rust fixture support uses ruzstd and compares decompressed length and frame checksum before bootstrapping the MFT.

## Fuzzing

The separate fuzz package has five targets: `boot_sector`, `file_record`, `attributes`, `runlist` and `typed_values`. From `crates/adamite-ntfs`, run:

```text
cargo install cargo-fuzz --locked
cargo +nightly fuzz build
```

Run `deno task fuzz-corpus` from the repository root to generate deterministic parser/core seeds, with a 64 KiB budget per target. Re-running this command replaces generated and evolved corpus files with those seeds. Then, from `crates/adamite-ntfs`, create `../../target/fuzz-smoke/adamite-ntfs/file_record/` before running:

```text
cargo +nightly fuzz run file_record ../../target/fuzz-smoke/adamite-ntfs/file_record/ fuzz/corpus/file_record/ -- -max_total_time=30 -max_len=262144 -timeout=2 -rss_limit_mb=2048
```

libFuzzer writes new inputs to the first corpus directory, so the scratch directory receives discoveries while the committed corpus remains a read-only seed source.

`deno task check --fuzz` requires a nonempty corpus for each target, builds both fuzz packages, and runs five parser targets plus core's `scan` target. Smoke runs write discovered inputs into fresh `target/fuzz-smoke/<package>/<target>/` directories and read committed corpora as seeds. Missing or empty corpora fail the command; run `deno task fuzz-corpus` to regenerate seeds.
