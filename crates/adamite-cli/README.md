# adamite-cli

`adamite-cli` builds the `adamite` binary. It loads NTFS images, Windows raw volumes and saved `.adm` snapshots, then prints reports or launches [adamite-tui](../adamite-tui/README.md). Build it from the repository root with `cargo build --release -p adamite-cli --locked`.

Source detection and loading, snapshot publication, status files, elevated scans, report types, and text and JSON rendering live in [adamite-access](../adamite-access/README.md). The CLI's `source` extension trait supplies terminal progress, scan limits and elevation policy; its `render` adapter converts command-line output flags. `error` maps access errors into the existing CLI messages, hints and exit codes. `cli::LimitArgs` converts parsed arguments into access `ScanOptions`. The CLI constructs final failure statuses with its existing messages and exit codes. `launcher::CliSources` delegates discovery, drive-snapshot matching and background work to access `SnapshotCatalog` and `ScanJob`. Its small job adapter supplies the existing Loading/Scanning titles and phase labels, transfers the completed output to the TUI, and displays job error messages.

## Commands and flags

| Command | Behavior and options |
| --- | --- |
| `adamite scan <SOURCE> [-o <FILE>] [--force] [--status-file <FILE>]` | Scan an image or drive. Without `-o`/`--output`, print the info report. With it, save a snapshot and print a one-line confirmation. `--force` requires an output file and allows replacement. `--status-file` requires an output file and writes progress and the outcome to a new JSON lines file. A snapshot source is a usage error. |
| `adamite info <SOURCE>` | Show volume metadata, tree totals and scan counters. |
| `adamite top <SOURCE> [--under PATH] [--ext EXT[,EXT…] \| --kind KIND] [--n 20]` | Show the largest primary files by allocation. Without query flags, read the stored 100-entry index; with `--under`, `--ext` or `--kind`, rank matching files in the subtree. `--n` accepts 1 through 100. |
| `adamite ext <SOURCE> [--under PATH] [--by extension\|kind] [--n 20]` | Show extension or file-category totals, ordered by allocation. The default is extension grouping from the stored index; `--under` restricts totals to a directory. `--n` must be at least 1. |
| `adamite ls <SOURCE> [PATH] [--offset 0] [--limit 50]` | List a directory's children in subtree allocation order. PATH defaults to `/`; an offset past the end returns an empty list. A file path is an error. |
| `adamite tree <SOURCE> [PATH] [--budget 60] [--min-share 0.5]` | Summarize a directory in tree order. PATH defaults to `/`. The budget accepts 1 through 400 and includes the root and collapsed rows. `--min-share` accepts a percentage from 0 through 100, converted to basis points by rounding down. |
| `adamite search <SOURCE> <PATTERN> [--under PATH] [--offset 0] [--limit 50]` | Search descendant names in stored-child pre-order. The offset counts matches; `--limit` accepts 1 through 500. |
| `adamite browse [SOURCE [PATH]]` | Without SOURCE, open Home. With SOURCE, load it and open the browser at PATH, default `/`. PATH requires SOURCE. Standard output must be a terminal; this is checked before source loading. |

`scan`, `info`, `top`, `ext`, `ls`, `tree` and `search` accept `--json` and `--bytes`. Text is the default; `--bytes` prints raw byte counts in text. JSON always uses bytes. `browse` rejects both flags. `--help` shows command help; `adamite --version` shows the version. Running `adamite` without a subcommand opens Home when both standard input and standard output are terminals; otherwise it prints the long help.

`top --ext` accepts comma-separated suffixes and may repeat, for example `--ext mp4,mkv --ext .webm`. One leading dot is removed and ASCII letters are lowercased. An empty value (`--ext ''`) includes names without a usable extension. `--ext` and `--kind` conflict. The accepted kind names are `video`, `audio`, `image`, `archive`, `disk-image`, `document`, `code`, `executable` and `other`; `ts` belongs to video. Rankings and totals count primary files, excluding duplicate hard links.

`search` uses case-insensitive globs over whole node names, with backslash escaping and wildcards that can match separators. A pattern without `*?[{` is matched as a substring, so `steam` finds `SteamLibrary`. Quote globs to keep the shell from expanding them. Patterns are limited to 256 bytes. Search includes directories, synthetic directories and duplicate hard links, and excludes the scope directory itself.

```sh
adamite top C.adm --under /Users --ext mp4,mkv
adamite top C.adm --kind video --n 50
adamite ext C.adm --under /Users --by kind
adamite search C.adm '*.iso' --under /Users --limit 50
adamite search C.adm steam --offset 50
```

## Home and saved scans

Run `adamite` or `adamite browse` to select a source in the terminal. Home lists NTFS drives on Windows, saved snapshots on either platform, and a final `Open file…` row for image or snapshot paths. Enter opens a drive's saved snapshot or scans it when none exists; `r` rescans the selected drive. A typed drive such as `D:` opens its saved snapshot when listed, or starts a drive scan when none is available. Images opened from Home are scanned without saving. In the browser, `o` returns to refreshed Home; `q` quits outside input lines. See the [TUI controls](../adamite-tui/README.md).

Drive scans save to `<snapshot directory>/<LETTER>.adm`, replacing the previous snapshot through the same synchronized temporary-file publication as `scan --force`. The directory is created before the first drive save:

| Platform | Snapshot directory |
| --- | --- |
| Windows | `snapshots\` next to `adamite.exe`, so deleting the program folder removes every saved scan. The folder must be writable by the current user. |
| Linux / macOS | `$XDG_DATA_HOME/adamite/snapshots`, or `~/.local/share/adamite/snapshots` when XDG_DATA_HOME is unset or relative. |

Snapshot loads, image scans and saves run on background threads. Unelevated Windows drive scans request UAC through the existing elevated child, then load its published snapshot on a worker. The progress page displays phase, records, bytes and rate without writing progress to standard error. Failures return to Home with their message. Quitting drops the job: an elevated child continues and publishes its saved result, while in-process work ends with the parent process.

`browse` accepts the scan-resource flags below for all jobs started from Home. The global `--trace-chrome FILE` also works without a subcommand and records background work and browser rendering.

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

Command-line image and supported drive scans show progress on standard error when it is a terminal, including `browse SOURCE` before the browser opens. Home jobs display progress inside the TUI. MFT streaming shows processed and planned records, percentage, binary bytes read, average logical read rate since scan start and ETA. Boot, discovery and later processing phases show a spinner with the phase label. Indicatif limits redraw frequency, and the display clears on success or failure before results or errors print. When standard error is piped or redirected, scans print the single line `scanning <source>…`.

Image scans record the current scan timestamp; Windows drive scans also obtain the volume label and free bytes. Queries on snapshots reuse their saved metadata.

For `ls`, `tree`, `browse` and `--under`, PATH is relative to the snapshot root and resolved by core's `Snapshot::resolve_path`; the CLI requires the result to be a directory. `--under` defaults to `/`. Both `/` and `\` separate components; empty components are skipped, so a leading separator is optional. `.` and `..` are literal names. Exact name matches take precedence; otherwise names are compared after Unicode lowercase expansion. Multiple matches produce `AmbiguousPath` with candidate NodeIds. A component such as `#1234` selects that decimal NodeId among the current directory's direct children. Synthetic directories such as `<NTFS metadata>` and `<orphaned>` use the same rules. Returned paths use `/` separators.

## Text and JSON output

Text reports start with the source and scan time, followed by aligned fields or columns. Sizes use binary units such as `12.0 GiB`, or integers with `--bytes`. `scan -o` prints only the save confirmation. Text and terminal names escape control characters as `\u{…}`; empty extensions display as `(none)`. Search rows show subtree sizes, file counts and full paths, with `/` marking directories, as in `ls`. The footer reads `showing 1–50 of 120 · next offset=50`; the final page omits the next offset. No matches prints `no matches`, and an offset past the end prints `showing 0 of N`.

Tree text starts with source, volume serial, scan timestamp, age and `sizes: on-disk allocation`. Its `alloc  share  name` rows use two spaces of indentation per depth. Shares are percentages of the requested root allocation, displayed to one decimal place. Children below the threshold collapse with their later siblings as `… N more (X)`, where N counts direct children and X sums their subtree allocation. A zero-allocation root ignores the threshold and reports zero shares. The budget reserves a collapsed row for each displayed directory's remaining children; a candidate expands only when the node and its remaining child and sibling groups fit. Budget one shows only the root. `truncated` in JSON is true when the budget prevents expansion.

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

| Command | Additional fields |
| --- | --- |
| `info`, `scan` without output | `volume: {bytes_per_sector, cluster_size, total_bytes, free_bytes}`, `tree: {nodes, files, total_size, total_alloc}`, `stats: {…}`. `command` is `info` or `scan` respectively. |
| `top` | `items: [{path, size, alloc, mtime_filetime}]`; modification FILETIME is a decimal string, including zero. |
| `ext` | `items: [{extension, count, total_size, total_alloc}]`; the empty extension is `""`. |
| `ext --by kind` | `items: [{kind, count, total_size, total_alloc}]`; `kind` is one of the file-category names above. |
| `search` | `items: [{path, name, kind, flags, size, alloc, total_size, total_alloc, file_count}]`, `total_matches`, `next_offset`. The next offset is `null` on the last page or past the end. |
| `ls` | `path`, `total_children`, `offset`, `limit`, `items: [{name, kind, flags, size, alloc, total_size, total_alloc, file_count}]`. `kind` is `"dir"` or `"file"`. |
| `tree` | `rows: [{path, name, depth, alloc, size, share, kind, count?}]`, `truncated`. `alloc` and `size` are subtree byte totals; `share` is a percentage. `kind` is `"node"` or `"collapsed"`; collapsed rows use the parent path, the name `…` and a direct-child `count`. |
| `scan` with output | `saved` (destination path), `bytes` (encoded snapshot length). |

`stats` includes all 26 counters, including zeros: `records_total`, `records_in_use`, `base_records`, `extension_records`, `unused`, `deleted`, `baad`, `corrupt_records`, `bad_fixup`, `invalid_fixup_metadata`, `invalid_signature`, `corrupt_headers`, `corrupt_attributes`, `type_storage_mismatches`, `corrupt_values`, `corrupt_runlists`, `out_of_volume_extents`, `invalid_attribute_lists`, `invalid_file_refs`, `invalid_utf16`, `rejected_extensions`, `corrupt_streams`, `orphans`, `cycle_nodes`, `hardlink_duplicates`, `metafiles`. Text info reports omit zero counters.

`ls.flags` and `search.items[].flags` contain the set names in bit order: `directory`, `reparse`, `sparse`, `compressed`, `hidden`, `system`, `hardlink_dup`, `synthetic`. For `ls` and `search`, `kind` is `"dir"` or `"file"`. `size`/`alloc` describe the node itself; `total_size`/`total_alloc` include its subtree. See [core accounting](../adamite-core/README.md) for stream and hard-link rules.

Errors use text on standard error, beginning with `error:` and followed by `caused by:` lines when a source exists. Failed commands leave standard output empty. A broken standard-output pipe ends successfully.

## Scan resource limits

All commands accept the same scan flags. Loading a saved snapshot ignores them. Only explicitly supplied resource flags are forwarded to an elevated scan; the child uses the same defaults for omitted flags.

| Flag | Default and meaning |
| --- | --- |
| `--profile background\|balanced\|max` | Balanced: 256 MiB/s and 1 MiB chunks. Background: 64 MiB/s and 1 MiB chunks. Max: uncapped reads and 4 MiB chunks. All profiles use one outstanding request. |
| `--max-read-rate <BYTES>` | Override the profile's read budget. K/M/G and KiB/MiB/GiB use powers of 1024, for example `128M` or `128MiB`. Zero and `unlimited` are rejected; select `--profile max` to uncap. |
| `--threads <N>` | Positive worker ceiling, default 1. The current scanner uses one thread for every ceiling; future parallel work must honour it. |
| `--no-elevate` | Scan Windows drives in the current process without requesting administrator access. Accepted for all sources and platforms. |

Adamite sleeps between reads to enforce the budget. For budgets larger than one chunk per second, the token bucket reserves one chunk from its refill rate so any one-second window stays within the requested budget. Very low custom budgets allow one indivisible chunk beyond the one-second budget. Rates count logical requested bytes; Windows sector alignment can expand native reads.

```text
adamite scan C: --profile background -o C.adm
adamite info volume.img --profile balanced --max-read-rate 128M --threads 4
adamite scan volume.img --profile max
```

## Saving snapshots

`scan -o` creates a same-directory temporary file named `.<file>.<pid>-<n>.tmp` with exclusive creation (up to 16 collision retries), streams the completed scan through the shared `adamite-io` adapter, patches the header CRC, synchronizes it, then closes it before publication. Encoding uses at most 1 MiB scratch. Snapshot loading uses two sequential passes and a fixed 1 MiB read buffer: verification first, then decoding into final columns and snapshot validation. Without `--force`, existing destinations are rejected before scanning. Directories and outputs resolving to the source image are also rejected.

Without `--force`, publication creates a hard link to the completed temporary file and removes the temporary name. This refuses a destination created concurrently. Filesystems without hard-link support return `UnsupportedNoClobber` and suggest `--force`. With `--force`, rename replaces the destination without deleting it first. Cleanup is attempted on failure; a failed temporary-name removal after successful hard-link publication emits a warning and is retried on drop.

`scan <SOURCE> -o <SNAPSHOT> --status-file <STATUS>` exclusively creates STATUS before scanning. An existing status file is left untouched and returns exit code 2. The command keeps its usual standard output and standard error. Each newline-terminated JSON object reports progress or the final outcome:

```json
{"progress":{"phase":"stream","records":100,"planned":200,"bytes":1048576}}
{"done":{"saved":"volume.adm","bytes":12345}}
```

Progress is flushed on phase changes and otherwise at most every 500 milliseconds. A failed command ends with `{"failed":{"message":"<error text>","exit_code":1}}`, using the error text the CLI prints after `error: `, including `caused by:` lines, and the same exit code. The status file remains available after the command exits.

## Exit codes and Windows elevation

| Code | Condition |
| --- | --- |
| 0 | Success, help, version or a broken standard-output pipe. |
| 2 | Invalid arguments, invalid or oversized search patterns, an existing status file, scanning an existing snapshot, a raw drive on a non-Windows platform, or `browse` without terminal output. |
| 3 | Snapshot encoding or decoding (`PersistError`). |
| 4 | Source/output permission denial, raw-volume access denial, declined elevation or elevation launch failure. |
| 1 | Other failures, including I/O, scan, allocation, path, output and terminal errors. |

Terminal errors return 1; snapshot codec errors take precedence over permission errors in their source chain. Loading a snapshot requires file access only. See [adamite-tui](../adamite-tui/README.md) for tabs, keys and rendering.

On Windows, `scan`, `info`, `top`, `ext`, `ls`, `tree`, `search` and `browse` automatically request UAC elevation for drive sources when the current process is unelevated. Before launching, the parent prints `requesting administrator access for <drive>…`. The elevated child scans and saves a snapshot while the parent displays progress, waits for the child to exit, then loads the snapshot and renders the requested result. `scan -o FILE` uses FILE after the normal destination checks; other commands use a unique `adamite-<pid>-<n>.adm` in the system temporary directory. The status file is `.<snapshot file>.<pid>-<n>.status.jsonl` beside the snapshot. The parent closes its reader before removing the status file and removes temporary snapshots after loading them.

The child receives `--no-elevate`, the explicitly supplied `--profile`, `--max-read-rate` and `--threads` options, and `--force` only for `scan -o FILE --force`. `--trace-chrome` profiles the parent and is not forwarded. Declining the prompt prints `error: elevation was declined` and returns 4. Other elevation failures include the system error and suggest `run with --no-elevate in an elevated terminal`. A child failure returns its reported message and exit code; an exit without a final status line returns 1 and names the child's exit code.

Use `--no-elevate` to scan in the current process. Access-denied drive errors then suggest `run without --no-elevate to request elevation, or use an elevated terminal`.
