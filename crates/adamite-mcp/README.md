# adamite-mcp

`adamite-mcp` serves ten tools over MCP: `snapshots`, `snapshot_info`, `get_tree`, `list_dir`, `top_files`, `ext_breakdown`, `search`, `node_info`, `scan_volume` and `scan_status`. It uses `adamite-access::SnapshotCatalog` for discovery, `Source::open_snapshot(...).load_observed(...)` for decoding, and the shared access reports and text tables for queries.

Start the server with:

```sh
adamite mcp --snapshot C.adm --snapshot-dir ./snapshots --cache 2 --stale-after 86400
```

| Option | Default and behavior |
| --- | --- |
| `--snapshot FILE` | May repeat; adds explicit saved snapshots to directory discovery. |
| `--snapshot-dir DIR` | Defaults to the platform [snapshot directory](../adamite-cli/README.md#home-and-saved-scans), also used for scan publication. |
| `--cache N` | Nonnegative machine-sized integer, default 2 retained outputs; zero shares only in-flight loads. |
| `--stale-after SECONDS` | Nonnegative u64 seconds, default 86400 (24 hours); age at or above the threshold is stale. |
| `--trace-chrome FILE` | Global profiling option; writes the trace separately from protocol stdout. |

For Claude Code, the repository provides a [plugin and disk-usage Skill](../../plugins/adamite/README.md) with installation, consent and permission rules. The binary is installed separately on PATH.

The server uses a current-thread Tokio runtime. Discovery, decoding and queries run in `spawn_blocking`, with two query permits and the cache's single process-wide load permit. The query permit stays with blocking work even if its response handler is cancelled. On stdin EOF, rmcp stops receiving requests and drains responses; runtime shutdown waits for all started blocking work before the command returns successfully.

## Public API

| Type | Behavior |
| --- | --- |
| `ServerOptions` | Combines `SnapshotOptions` and a `Duration` staleness threshold, defaulting to 24 hours. |
| `McpServer` | `new(options)` initializes discovery and cache; `serve_stdio()` owns the current-thread runtime and serves stdin/stdout. It also implements rmcp `ServerHandler` for an embedding application or in-process transport. |
| `SnapshotOptions` | Sets explicit snapshot paths, an optional directory override and cache capacity, defaulting to two entries. |
| `SnapshotStore` | `new(options)` resolves absolute paths and creates the default snapshot directory. `directory()` returns that directory. `snapshots()` refreshes discovery, `select(snapshot)` chooses a path, and `load(snapshot)` selects and loads an `Arc<ScanOutput>`. |
| `SnapshotInfo` | Contains the filename, absolute path, byte length, modification time, cached `ScanTime` and any per-entry metadata error. Unnamed directory enumeration errors have no name or path. |
| `SnapshotCache` | `default()` retains two outputs; `new(capacity)` changes the limit. `load(path)` shares decoded outputs by canonical path and reloads when length or modification time changes. Zero capacity shares in-flight work without retaining completed outputs. |
| `McpError` | Preserves selection, decoding, permission and other I/O failures. `code()` returns `no_snapshot`, `unsupported_source`, `ambiguous_snapshot`, `corrupt_snapshot`, `permission_denied`, `path_not_found`, `ambiguous_path`, `not_a_directory`, `invalid_argument`, `scan_busy`, `elevation_declined`, `scan_failed` or `internal_error`. `next_step()` returns recovery instructions, including the absolute manual scan destination for a missing snapshot. |

Discovery merges explicit files and direct `.adm` files from the catalog, following symlinks and omitting subdirectories. It deduplicates identical absolute paths, refreshes metadata on every call and keeps unreadable entries as error rows. Cached scan timestamps appear only while the file metadata still matches. A missing overridden directory is empty; failure to create the default directory is tolerated, and subsequent calls retry discovery.

Selection accepts an `.adm` path, a single ASCII volume letter with an optional colon, or an omitted selection when exactly one file is discovered. Volume matching ignores ASCII case. Several matches return their absolute candidate paths.

Concurrent requests for the same canonical path share one in-flight result. A process-wide permit serializes all snapshot decoding, and decoding holds no cache lock. The least recently used output is evicted only when the cache holds its sole strong reference. Active outputs can temporarily exceed capacity; later cache accesses trim unused entries. Failed loads are removed so the next request retries.

Scan jobs use `adamite-access::ScanJob`, `ScanOptions`, `ElevatedScan` and `SnapshotFile`. MCP validates sources and retains job progress and outcomes. The access layer performs scanning, status-file writing and atomic snapshot publication.

## Tool parameters and output

Every query tool accepts `snapshot` (an optional saved `.adm` path or volume letter) and `format` (`text` by default, or `json`). `snapshots` lists all discovered files when selection is omitted; an explicit selection filters the catalog. It reads metadata without loading snapshots and shows scan timestamps only for matching cached outputs. Its text rows include absolute paths, file lengths, RFC 3339 modification times and known scan times. Its JSON `items` contain `name`, `path`, `byte_len`, `modified`, `scanned_at`, `age_seconds`, `stale` and a nullable per-entry `error`. An empty catalog is a successful empty listing.

| Tool | Parameters beyond common `format` and query `snapshot` |
| --- | --- |
| `snapshots` | None; omitted `snapshot` lists all files, supplied `snapshot` filters the catalog. |
| `snapshot_info` | None; shows volume metadata, tree totals and scan counters. |
| `get_tree` | `path="/"`, `budget=60` (1–400 rows including root and collapsed groups), `min_share_percent=0.5` (finite 0–100 percentage, rounded down to basis points). |
| `list_dir` | `path="/"`, `offset=0` (nonnegative machine-sized integer), `limit=50` (1–500); offset + limit must fit. |
| `top_files` | `n=20` (1–100), optional `under`, optional `ext` array or `kind` string, mutually exclusive. |
| `ext_breakdown` | `n=20` (1–200), optional `under`, `by="extension"` or `"kind"`. |
| `search` | Required `pattern` (at most 256 bytes), optional `under`, `offset=0` (nonnegative machine-sized integer), `limit=50` (1–500). |
| `node_info` | Required `path`, accepting a file or directory. |
| `scan_volume` | Required `volume`, `profile="balanced"` (`background`, `balanced`, `max`); accepts `format`, no `snapshot`. |
| `scan_status` | Optional `volume` matching the current or last job; accepts `format`, no `snapshot`. |

`under` defaults to `/`. `ext` accepts suffixes with or without a leading dot, lowercases ASCII letters, and uses an empty string for files without an extension, for example `["mp4", ".mkv"]`. `kind` accepts `video`, `audio`, `image`, `archive`, `disk-image`, `document`, `code`, `executable`, `other`. These categories are inferred from extensions. Search uses case-insensitive whole-name globs (`*`, `?`, `[abc]`, `{a,b}`), with backslash escaping; input without `*?[{` matches a substring. Search includes directories and duplicate hard links; rankings and grouped totals count primary files only.

Paths start with `/` and use `/` separators in results. Input accepts `/` or `\` separators and resolves exact names before Unicode lowercase matches; `#id` selects a direct child. `list_dir` preserves the access report's allocation order and fields. A page past the end is empty. Text pages with remaining children end with `showing A–B of N · next offset=B`; the last page omits this footer. `get_tree` uses two spaces of indentation per depth with `alloc  share  name` columns, shares to one decimal place, and collapsed rows named `… N more (X)`. Its budget/collapse rules are described in the [core README](../adamite-core/README.md#snapshot-and-errors).

Query text begins with `<snapshot file> · serial <hex> · scanned <RFC 3339 | unknown> (<age>) · sizes: on-disk allocation`. Ages use the largest whole unit (`45s`, `12m`, `5h`, `3d`). Missing, future and unrepresentable scan ages are unknown.

JSON is one `adamite/1` object returned identically as a text block and MCP `structuredContent`. The wrapper extends the access report's `snapshot` with `path`, `age_seconds` and `stale`. Unknown ages make `age_seconds` and `stale` null. The catalog has no selected snapshot, so its top-level `snapshot` fields are null; each item carries its own age. Successful reports include `truncated` and a nullable `next_offset`; `list_dir` and `search` set the next offset while results remain. The response builder assembles reports and paging before wire encoding. Every encoded MCP result is capped at 32 KiB, including the wrapper and duplicated structured content, by dropping whole rows/items and setting `truncated`. Oversized strings are shortened in the middle at UTF-8 boundaries when needed. Non-paginated tools leave `next_offset` null; narrow `path` or `under` for a truncated tree or ranking.

Shared report fields are documented in the [CLI output contract](../adamite-cli/README.md#text-and-json-output), with MCP tool names in `command`. `node_info` adds `path`, `node_id`, `hard_link_count`, nullable `mtime_filetime` and `modified_at`, plus the directory-listing node fields (`name`, `kind`, flags and self/subtree sizes). `get_tree` rows use subtree `alloc`/`size` and percentage `share`. Scan responses carry `status` and the job fields described below instead of the loaded-snapshot envelope.

The query tools and `scan_status` advertise `readOnlyHint: true`, `idempotentHint: true` and `openWorldHint: false`. `scan_volume` advertises `readOnlyHint: false`, `destructiveHint: false`, `idempotentHint: false` and `openWorldHint: false`.

`scan_volume` takes `volume`, `profile` (`background`, `balanced` by default, or `max`) and `format`. A drive is exactly one ASCII letter with an optional colon, normalized to uppercase, and requires Windows. Other sources must be existing regular image files; paths are canonicalized and `.adm` files are rejected with instructions to query the snapshot. Only the validated drive or canonical image path and enum-derived profile enter the scan arguments. Snapshots replace `<snapshot-dir>/<stem>.adm`; progress uses `<snapshot-dir>/.<stem>.status.jsonl`.

Images and drives with administrator access scan on access-layer workers. Other Windows drive scans launch an elevated child; `scan_volume` waits for the UAC decision on a blocking worker, then returns `running` with `volume` and `snapshot.path`, or `elevation_declined`. Ask the user whether to retry a declined prompt. Another launch while scanning returns `scan_busy` and directs the caller to `scan_status`. The launch permit stays with blocking work when a request is cancelled.

`scan_status` accepts optional `volume` and `format`. Without a previous job it returns `idle`. A supplied volume that does not match the current or last job returns `invalid_argument`; omit it or pass the job's drive or image path. While running, JSON includes `phase`, `records_processed`, `records_planned`, `percentage`, `bytes_read` and `elapsed_seconds`. The percentage is `100 * records_processed / records_planned` when the planned count is positive, otherwise null. Completion returns `done` and `snapshot.path`; subsequent discovery and queries see the published file. Failures return `scan_failed` with the access-layer message and child exit code when available; in-process failures have no child exit code. Repeated status calls retain the terminal outcome, and the next scan replaces it. The server closes status handles before removing a previous job's status file for a new scan.

Elevated children continue running and publishing to their explicit destination after server shutdown. Dropping the process owner closes the Windows process handle without terminating or waiting for the child. Server instructions describe consent, polling and query steps.

## Errors

Malformed requests, unknown tools and parameter deserialization failures are protocol errors. Other failures set MCP `isError: true`. Text errors contain exactly two lines: `error[<code>]: <what happened>` and `next: <exact step>`. JSON errors wrap that same two-line string as `{"schema":"adamite/1","error":"..."}`, identically in text and structured content. A failed call leaves the server running.

| Code | Condition and recovery |
| --- | --- |
| `no_snapshot` | No matching saved snapshot; ask for consent, then call `scan_volume`. `next` also supplies a manual scan command with the absolute destination and `--force` when it exists. |
| `unsupported_source` | Non-`.adm` query source, `.adm` scan source, or a drive scan outside Windows; `next` supplies an image scan command or directs the caller to query the existing snapshot. |
| `elevation_declined` | UAC was declined; ask whether to retry and wait for agreement. |
| `scan_busy` | Another scan is active; call `scan_status`. |
| `scan_failed` | Scan worker or child failed; inspect the reported message and exit code (unavailable for in-process failures). |
| `ambiguous_snapshot` | Several selections match; pass one of the absolute candidate paths. |
| `path_not_found` | A component is missing; call `list_dir` on the deepest resolved directory from `next`. |
| `ambiguous_path` | Several children match; retry with a returned identifier path containing `/#id`. |
| `not_a_directory` | A directory query selected a file; use `node_info`. |
| `corrupt_snapshot` | Decoding failed; rescan the original source and replace the snapshot after consent. The manual command uses `--force`. |
| `permission_denied` | File access was denied; check permissions or rescan into the snapshot directory. |
| `invalid_argument` | Parameter outside its accepted range or values, invalid glob, invalid scan source or status volume mismatch; follow the correction in `next`. |
| `internal_error` | Other I/O or synchronization failure; check the reported path and cause, then retry. |

## Verification

Run `cargo test -p adamite-mcp --locked` for discovery, selection, cache invalidation, eviction, concurrent loading and retry coverage, plus an in-process rmcp client over `tokio::io::duplex`. The wire tests cover initialization, all ten tool names and annotations, text/JSON output, paging, unknown ages, protocol errors and recoverable tool failures using `adamite-fixtures` snapshots and images. Scan tests cover publication, discovery, queries, progress, busy rejection, retained terminal outcomes, replacement, source validation and corrupt images. A unit test checks typed elevation-decline mapping; real Windows UAC acceptance and refusal require manual verification. Run `deno task check` from the repository root for formatting, host and Windows Clippy, workspace tests, documentation and dependency checks.
