# adamite-mcp

`adamite-mcp` discovers and selects saved `.adm` snapshots and shares decoded outputs between blocking callers. It uses `adamite-access::SnapshotCatalog` for directory discovery and `Source::open_snapshot(...).load_observed(...)` for decoding. Future MCP tools can call this API from a blocking worker pool.

## Public API

| Type | Behavior |
| --- | --- |
| `SnapshotOptions` | Sets explicit snapshot paths, an optional directory override and cache capacity, defaulting to two entries. |
| `SnapshotStore` | `new(options)` resolves absolute paths and creates the default snapshot directory. `directory()` returns that directory. `snapshots()` refreshes discovery, `select(snapshot)` chooses a path, and `load(snapshot)` selects and loads an `Arc<ScanOutput>`. |
| `SnapshotInfo` | Contains the filename, absolute path, byte length, modification time, cached `ScanTime` and any per-entry metadata error. Unnamed directory enumeration errors have no name or path. |
| `SnapshotCache` | `default()` retains two outputs; `new(capacity)` changes the limit. `load(path)` shares decoded outputs by canonical path and reloads when length or modification time changes. Zero capacity shares in-flight work without retaining completed outputs. |
| `McpError` | Preserves selection, decoding, permission and other I/O failures. `code()` returns `no_snapshot`, `unsupported_source`, `ambiguous_snapshot`, `corrupt_snapshot`, `permission_denied` or `internal_error`. `next_step()` returns recovery instructions, including the absolute manual scan destination for a missing snapshot. |

Discovery merges explicit files and direct `.adm` files from the catalog, following symlinks and omitting subdirectories. It deduplicates identical absolute paths, refreshes metadata on every call and keeps unreadable entries as error rows. Cached scan timestamps appear only while the file metadata still matches. A missing overridden directory is empty; failure to create the default directory is tolerated, and subsequent calls retry discovery.

Selection accepts an `.adm` path, a single ASCII volume letter with an optional colon, or an omitted selection when exactly one file is discovered. Volume matching ignores ASCII case. Several matches return their absolute candidate paths.

Concurrent requests for the same canonical path share one in-flight result. A process-wide permit serializes all snapshot decoding, and decoding holds no cache lock. The least recently used output is evicted only when the cache holds its sole strong reference. Active outputs can temporarily exceed capacity; later cache accesses trim unused entries. Failed loads are removed so the next request retries.

Scan jobs remain owned by `adamite-access::ScanJob`; later scan tools will use that API.

## Verification

Run `cargo test -p adamite-mcp --locked` for discovery, selection, cache invalidation, eviction, concurrent loading and retry coverage using `adamite-fixtures` snapshots. Run `deno task check` from the repository root for formatting, host and Windows Clippy, workspace tests, documentation and dependency checks.
