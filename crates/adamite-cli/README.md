# adamite-cli

`adamite-cli` builds the `adamite` binary. It loads NTFS images, Windows raw volumes and saved `.adm` snapshots, then prints reports or launches [adamite-tui](../adamite-tui/README.md). Build it from the repository root with `cargo build --release -p adamite-cli --locked`.

## Commands and flags

| Command                                                | Behavior and options                                                                                                                                                                                                              |
| ------------------------------------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `adamite scan <SOURCE> [-o <FILE>] [--force]`          | Scan an image or drive. Without `-o`/`--output`, print the info report. With it, save a snapshot and print a one-line confirmation. `--force` requires an output file and allows replacement. A snapshot source is a usage error. |
| `adamite info <SOURCE>`                                | Show volume metadata, tree totals and scan counters.                                                                                                                                                                              |
| `adamite top <SOURCE> [--n 20]`                        | Show files from the precomputed 100-entry index, ordered by allocation. `--n` accepts 1 through 100.                                                                                                                              |
| `adamite ext <SOURCE> [--n 20]`                        | Show extension groups in stored allocation order. `--n` must be at least 1.                                                                                                                                                       |
| `adamite ls <SOURCE> [PATH] [--offset 0] [--limit 50]` | List a directory's children in subtree allocation order. PATH defaults to `/`; an offset past the end returns an empty list. A file path is an error.                                                                             |
| `adamite browse <SOURCE> [PATH]`                       | Open the terminal browser at a directory, default `/`. Standard output must be a terminal; this is checked before source loading.                                                                                                 |

`scan`, `info`, `top`, `ext` and `ls` accept `--json` and `--bytes`. Text is the default; `--bytes` prints raw byte counts in text. JSON always uses bytes. `browse` rejects both flags. `--help` shows command help; `adamite --version` shows the version. Running `adamite` without a subcommand prints help.

## Chrome tracing

Every subcommand accepts the global `--trace-chrome <FILE>` option before or after its name. For example:

```sh
adamite --trace-chrome scan.json scan volume.img -o volume.adm
adamite info volume.adm --trace-chrome load.json
```

Open the JSON file in Perfetto or `chrome://tracing` to inspect scan stages, I/O batches, snapshot encoding/decoding and rendering. Each span ends with a `heap_usage` event whose `args` identify the span and report `heap_current`, `heap_peak`, `heap_delta` and `process_peak` in requested bytes. `heap_peak` covers that span's open lifetime, and `process_peak` is cumulative for the process; the signed delta is the change in live bytes since that span opened. Measurements include other threads and profiling overhead. The trace is finished when the command returns, including command errors. Without this option, the binary installs no tracing subscriber.

## Sources and paths

SOURCE is classified in this order:

1. Exactly an ASCII letter and `:`, such as `C:` or `c:`, selects a Windows raw drive. Other platforms return exit code 2. `C:\` and longer arguments are file paths.
2. A file with the eight-byte `.adm` magic (`ADAMITE\0`), or an `.adm` extension ignoring ASCII case, selects a snapshot. Decoding failures return exit code 3 without attempting an image scan.
3. Any other file selects an image, starting at NTFS volume byte zero. Decompress images first; extract the volume from partitioned disk or VHD/VHDX containers before scanning.

Image and supported drive scans print `scanning <source>…` to standard error. Image scans record the current scan timestamp; Windows drive scans also obtain the volume label and free bytes. Queries on snapshots reuse their saved metadata.

For `ls` and `browse`, PATH is relative to the snapshot root. Both `/` and `\` separate components; empty components are skipped, so a leading separator is optional. `.` and `..` are literal names. Exact name matches take precedence; otherwise names are compared after Unicode lowercase expansion. Multiple matches produce `AmbiguousPath` with candidate NodeIds. A component such as `#1234` selects that decimal NodeId among the current directory's direct children. Synthetic directories such as `<NTFS metadata>` and `<orphaned>` use the same rules. Returned paths use `/` separators.

## Text and JSON output

Text reports start with the source and scan time, followed by aligned fields or columns. Sizes use binary units such as `12.0 GiB`, or integers with `--bytes`. `scan -o` prints only the save confirmation. Text and terminal names escape control characters as `\u{…}`; empty extensions display as `(none)`.

Each successful non-interactive `--json` invocation writes one object followed by a newline. The common envelope is:

```json
{
  "schema": "adamite/1",
  "command": "info",
  "snapshot": {
    "source": "C.adm",
    "scanned_at": "2026-11-10T12:00:00Z",
    "scanned_at_filetime": "134387856000000000",
    "volume_serial": "0123456789abcdef",
    "label": null
  },
  "units": { "size": "bytes" }
}
```

The example shows the envelope; command fields are added at the top level. `source` is the supplied argument converted to text with replacement for invalid Unicode. `scanned_at` is RFC 3339 UTC at seconds precision, or `null` for absent or unrepresentable timestamps. `scanned_at_filetime` is a decimal string of 100-nanosecond ticks since 1601-01-01, or `null` when absent. `volume_serial` has sixteen lowercase hexadecimal digits. `label` and `volume.free_bytes` may be `null`. Byte counts are JSON integers; field names are snake_case and stable within `adamite/1`.

| Command                       | Additional fields                                                                                                                                                               |
| ----------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `info`, `scan` without output | `volume: {bytes_per_sector, cluster_size, total_bytes, free_bytes}`, `tree: {nodes, files, total_size, total_alloc}`, `stats: {…}`. `command` is `info` or `scan` respectively. |
| `top`                         | `items: [{path, size, alloc, mtime_filetime}]`; modification FILETIME is a decimal string, including zero.                                                                      |
| `ext`                         | `items: [{extension, count, total_size, total_alloc}]`; the empty extension is `""`.                                                                                            |
| `ls`                          | `path`, `total_children`, `offset`, `limit`, `items: [{name, kind, flags, size, alloc, total_size, total_alloc, file_count}]`. `kind` is `"dir"` or `"file"`.                   |
| `scan` with output            | `saved` (destination path), `bytes` (encoded snapshot length).                                                                                                                  |

`stats` includes all 26 counters, including zeros: `records_total`, `records_in_use`, `base_records`, `extension_records`, `unused`, `deleted`, `baad`, `corrupt_records`, `bad_fixup`, `invalid_fixup_metadata`, `invalid_signature`, `corrupt_headers`, `corrupt_attributes`, `type_storage_mismatches`, `corrupt_values`, `corrupt_runlists`, `out_of_volume_extents`, `invalid_attribute_lists`, `invalid_file_refs`, `invalid_utf16`, `rejected_extensions`, `corrupt_streams`, `orphans`, `cycle_nodes`, `hardlink_duplicates`, `metafiles`. Text info reports omit zero counters.

`ls.flags` contains the set names in bit order: `directory`, `reparse`, `sparse`, `compressed`, `hidden`, `system`, `hardlink_dup`, `synthetic`. `size`/`alloc` describe the node itself; `total_size`/`total_alloc` include its subtree. See [core accounting](../adamite-core/README.md) for stream and hard-link rules.

Errors use text on standard error, beginning with `error:` and followed by `caused by:` lines when a source exists. Failed commands leave standard output empty. A broken standard-output pipe ends successfully.

## Scan resource limits

All six commands accept the same scan flags. Loading a saved snapshot ignores them.

| Flag | Default and meaning |
|---|---|
| `--profile background\|balanced\|max` | Balanced: 256 MiB/s and 1 MiB chunks. Background: 64 MiB/s and 1 MiB chunks. Max: uncapped reads and 4 MiB chunks. All profiles use one outstanding request. |
| `--max-read-rate <BYTES>` | Override the profile's read budget. K/M/G and KiB/MiB/GiB use powers of 1024, for example `128M` or `128MiB`. Zero and `unlimited` are rejected; select `--profile max` to uncap. |
| `--threads <N>` | Positive worker ceiling, default 1. The current scanner uses one thread for every ceiling; future parallel work must honour it. |

Adamite sleeps between reads to enforce the budget. For budgets larger than one chunk per second, the token bucket reserves one chunk from its refill rate so any one-second window stays within the requested budget. Very low custom budgets allow one indivisible chunk beyond the one-second budget. Rates count logical requested bytes; Windows sector alignment can expand native reads.

```text
adamite scan C: --profile background -o C.adm
adamite info volume.img --profile balanced --max-read-rate 128M --threads 4
adamite scan volume.img --profile max
```

## Saving snapshots

`scan -o` encodes the completed scan, creates a same-directory temporary file named `.<file>.<pid>-<n>.tmp` with exclusive creation (up to 16 collision retries), writes and synchronizes it, then closes it before publication. Without `--force`, existing destinations are rejected before scanning. Directories and outputs resolving to the source image are also rejected.

Without `--force`, publication creates a hard link to the completed temporary file and removes the temporary name. This refuses a destination created concurrently. Filesystems without hard-link support return `UnsupportedNoClobber` and suggest `--force`. With `--force`, rename replaces the destination without deleting it first. Cleanup is attempted on failure; a failed temporary-name removal after successful hard-link publication emits a warning and is retried on drop.

## Exit codes and Windows elevation

| Code | Condition                                                                                                                     |
| ---- | ----------------------------------------------------------------------------------------------------------------------------- |
| 0    | Success, help, version or a broken standard-output pipe.                                                                      |
| 2    | Invalid arguments, scanning an existing snapshot, a raw drive on a non-Windows platform, or `browse` without terminal output. |
| 3    | Snapshot encoding or decoding (`PersistError`).                                                                               |
| 4    | Source/output permission denial or raw-volume access denial. Drive failures include a hint to use an elevated terminal.       |
| 1    | Other failures, including I/O, scan, allocation, path, output and terminal errors.                                            |

Terminal errors return 1; snapshot codec errors take precedence over permission errors in their source chain. To inspect a Windows drive, run `adamite scan C: -o C.adm` in an elevated terminal once, then run `adamite browse C.adm` or the report commands in an ordinary terminal that can read the saved file. Loading a snapshot requires file access only. See [adamite-tui](../adamite-tui/README.md) for tabs, keys and rendering.
