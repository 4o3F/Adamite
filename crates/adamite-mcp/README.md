# adamite-mcp

`adamite-mcp` serves the `snapshots`, `snapshot_info` and `list_dir` tools over MCP. It uses `adamite-access::SnapshotCatalog` for discovery, `Source::open_snapshot(...).load_observed(...)` for decoding, and the shared access reports and text tables for queries.

Start the server with:

```sh
adamite mcp --snapshot C.adm --snapshot-dir ./snapshots --cache 2 --stale-after 86400
```

`--snapshot FILE` may repeat. `--snapshot-dir DIR` defaults to the platform snapshot directory documented in the [CLI README](../adamite-cli/README.md#home-and-saved-scans). `--cache N` defaults to two retained outputs. `--stale-after SECONDS` accepts a nonnegative integer number of seconds and defaults to `86400` (24 hours). The global `--trace-chrome FILE` records profiling without writing logs to protocol stdout.

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
| `McpError` | Preserves selection, decoding, permission and other I/O failures. `code()` returns `no_snapshot`, `unsupported_source`, `ambiguous_snapshot`, `corrupt_snapshot`, `permission_denied`, `path_not_found`, `ambiguous_path`, `not_a_directory`, `invalid_argument` or `internal_error`. `next_step()` returns recovery instructions, including the absolute manual scan destination for a missing snapshot. |

Discovery merges explicit files and direct `.adm` files from the catalog, following symlinks and omitting subdirectories. It deduplicates identical absolute paths, refreshes metadata on every call and keeps unreadable entries as error rows. Cached scan timestamps appear only while the file metadata still matches. A missing overridden directory is empty; failure to create the default directory is tolerated, and subsequent calls retry discovery.

Selection accepts an `.adm` path, a single ASCII volume letter with an optional colon, or an omitted selection when exactly one file is discovered. Volume matching ignores ASCII case. Several matches return their absolute candidate paths.

Concurrent requests for the same canonical path share one in-flight result. A process-wide permit serializes all snapshot decoding, and decoding holds no cache lock. The least recently used output is evicted only when the cache holds its sole strong reference. Active outputs can temporarily exceed capacity; later cache accesses trim unused entries. Failed loads are removed so the next request retries.

Scan jobs remain owned by `adamite-access::ScanJob`; later scan tools will use that API.

## Tool parameters and output

Every tool accepts `snapshot` (an optional saved `.adm` path or volume letter) and `format` (`text` by default, or `json`). `snapshots` lists all discovered files when selection is omitted; an explicit selection filters the catalog. It reads metadata without loading snapshots and shows scan timestamps only for matching cached outputs. Its text rows include absolute paths, file lengths, RFC 3339 modification times and known scan times. Its JSON `items` contain `name`, `path`, `byte_len`, `modified`, `scanned_at`, `age_seconds`, `stale` and a nullable per-entry `error`. An empty catalog is a successful empty listing.

`snapshot_info` shows volume metadata, tree totals and scan counters. `list_dir` takes `path` (default `/`), `offset` (default `0`) and `limit` (default `50`, allowed `1` through `500`). Paths start with `/` and use `/` separators. `list_dir` preserves the access report's allocation order and fields. A page past the end is empty. Text pages with remaining children end with `showing A–B of N · next offset=B`; the last page omits this footer.

Query text begins with `<snapshot file> · serial <hex> · scanned <RFC 3339 | unknown> (<age>) · sizes: on-disk allocation`. Ages use the largest whole unit (`45s`, `12m`, `5h`, `3d`). Missing, future and unrepresentable scan ages are unknown.

JSON is one `adamite/1` object returned identically as a text block and MCP `structuredContent`. The wrapper extends the access report's `snapshot` with `path`, `age_seconds` and `stale`. Unknown ages make both age fields null. The catalog has no selected snapshot, so its top-level `snapshot` fields are null; each item carries its own age. Reports include `truncated: false` and a nullable `next_offset`; `list_dir` sets the next offset while children remain. The response builder assembles reports and paging before wire encoding. The 32 KiB item cap and remaining query tools are scheduled for M4c; scan tools are scheduled for M4d.

All three tools advertise `readOnlyHint: true`, `idempotentHint: true` and `openWorldHint: false`. Server instructions describe the complete planned workflow and consent rules, including the later tools.

Malformed requests, unknown tools and parameter deserialization failures are protocol errors. Other failures set MCP `isError: true`. Text errors contain exactly two lines: `error[<code>]: <what happened>` and `next: <exact step>`. JSON errors wrap that same two-line string as `{"schema":"adamite/1","error":"..."}`, identically in text and structured content. Missing paths suggest the deepest resolved directory for `list_dir`; ambiguous paths suggest identifier paths ending in `/#id`. These identifier paths can be passed directly to retry the call. A failed call leaves the server running.

## Verification

Run `cargo test -p adamite-mcp --locked` for discovery, selection, cache invalidation, eviction, concurrent loading and retry coverage, plus an in-process rmcp client over `tokio::io::duplex`. The wire tests cover initialization, exact tool names and annotations, text/JSON output, paging, unknown ages, protocol errors and recoverable tool failures using `adamite-fixtures` snapshots. Run `deno task check` from the repository root for formatting, host and Windows Clippy, workspace tests, documentation and dependency checks.
