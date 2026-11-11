# Plan: MCP server and disk-usage Skill

## Why

- Agents can only reach Adamite by running CLI commands. No CLI command answers "where did the space go" within a context budget.
- ARCHITECTURE §4.2–4.3 plans a budgeted summary and MCP tools; neither exists.
- Snapshot queries stop at child listings and the filtered queries from [query-tui.md](query-tui.md). Path resolution and budgeted summaries are missing.
- A drive scan needs administrator rights, but the agent host and its MCP server run unelevated; today the user must open an elevated terminal by hand.
- Nothing tells an agent the workflow: scan (with the user's consent), then query the saved snapshot, and never delete on its own.

## What Changes

- Core gains path resolution and a budgeted summary. Ranking, extension and kind totals, and search come from [query-tui.md](query-tui.md).
- A new `adamite-access` crate holds source loading, reports and text rendering shared by the CLI and MCP.
- The CLI gains a `tree` command.
- A new `adamite-mcp` library serves snapshot query tools and scan jobs over stdio, started by `adamite mcp`.
- MCP `scan_volume` requests drive scans through the UAC prompt from [elevation.md](elevation.md).
- The repository ships a Claude Code plugin with the MCP registration and a `disk-usage` Skill.

## Scope

In scope: the core queries, `adamite-access`, the CLI additions, the `adamite-mcp` library and `adamite mcp` subcommand, the plugin and Skill, tests, READMEs and ARCHITECTURE §1, §4 and §6.

Out of scope:
- `cancel_scan`; it needs driver cancellation, and an unelevated server cannot terminate an elevated child.
- CLI elevation, covered by [elevation.md](elevation.md).
- The planned elevated service and daemon.
- `top_dirs`; `get_tree` covers it.
- Regex search.
- MCP `outputSchema` and resources.
- HTTP transport.
- Query cancellation.
- A metadata-only `.adm` read.

## Design decisions

1. D1. Binary shape and runtime.
   - The `adamite-mcp` library owns the server, tools, discovery and cache.
   - The CLI's `mcp` subcommand runs it, dispatched in `main` before stdout is locked.
   - The release still ships one `adamite` binary, so packaging is unchanged.
   - Dependencies:
     - `rmcp` 3.9 with default features off and `server`, `macros` and `transport-io` on; dev-dependencies add `client`.
     - `schemars` 1.
     - Tokio with the current-thread runtime.
   - Loads and queries run in `spawn_blocking`.
   - One load permit covers all paths, and concurrent requests for the same path share one in-flight load. Two query permits limit concurrent queries.
   - On stdin EOF the server stops accepting requests, lets running blocking work finish and exits 0.
   - The global `--trace-chrome` option works with `mcp`. Nothing but protocol messages reaches stdout.
2. D2. Shared access crate. `adamite-access` takes from the CLI:
   - `Source` with `open`, `open_snapshot` (`.adm` only, never falling back to a scan) and `load_observed(limits, observer)`. The CLI passes its progress view as the observer.
   - Every report and the `Envelope`.
   - Text rendering (`write_text`).
   - `SourceError` and `ReportError`.

   The CLI keeps clap, exit codes, the terminal progress view, stdout handling and snapshot saving. Existing CLI text, JSON and exit codes are byte-for-byte unchanged.
3. D3. Core queries. Each query is a method on `Snapshot`. Core stays sans-io and panic-free, with fallible allocation (`try_reserve`; no infallible `collect` or `to_owned` on query paths). Errors use snafu `QueryError`.

   | Method | Behavior |
   |---|---|
   | `resolve_path(&str) -> Result<NodeId, QueryError>` | Replaces the CLI `Locator` with the same rules: `/` or `\` separators, exact name first, then Unicode lowercase, `#id` for a direct child. An ambiguous match returns candidate NodeIds. |
   | `summarize(root, budget, min_share_bp)` | Implements §4.2 with a lazy candidate heap over the pre-sorted children; memory is O(budget). See the rules below. |

   Rules for `summarize`:
   - `budget` counts every emitted row, including the root and collapsed rows.
   - The share denominator is the root's `total_alloc`. A child below `min_share_bp` collapses together with its later siblings into one row with the count and summed alloc.
   - A zero-alloc root disables collapsing, so only the budget limits the rows.
   - Rows come out in tree order, not heap order.

   `rank_files`, `extension_stats_under`, `kind_stats`, `search`, `FileFilter` and `NamePattern` come from [query-tui.md](query-tui.md) D1.
4. D4. CLI parity.
   - `adamite tree <SOURCE> [PATH] [--budget 60] [--min-share 0.5]`
     - `--budget` accepts 1–400.
     - `--min-share` is a percentage from 0 to 100; it is converted to basis points, rounding down.
   - `search`, `top --under/--ext/--kind` and `ext --under/--by` come from [query-tui.md](query-tui.md) D2.

   All of them support text and `--json` (`adamite/1`). New reports may carry `truncated` and `next_offset`; existing reports gain no fields. The Skill's CLI fallback uses these commands.
5. D5. MCP tools.
   - Query tools set the annotations `readOnlyHint`, `idempotentHint` and `!openWorldHint`. `scan_volume` sets `readOnlyHint: false` and `destructiveHint: false`.
   - Every tool takes an optional `snapshot` and `format: text|json`, default text.
   - Limits match the CLI, and `min_share_percent` uses the same unit as `--min-share`.

   | Tool | Parameters (default, limit) | Built on |
   |---|---|---|
   | `snapshots` | none | discovery and cache |
   | `snapshot_info` | — | info report |
   | `get_tree` | `path="/"`, `budget=60` (1–400), `min_share_percent=0.5` (0–100) | `summarize` |
   | `list_dir` | `path="/"`, `offset=0`, `limit=50` (1–500) | ls report |
   | `top_files` | `n=20` (1–100), `under?`, `ext?` | `rank_files` |
   | `ext_breakdown` | `n=20` (1–200), `under?` | extension stats |
   | `search` | `pattern`, `under?`, `offset=0`, `limit=50` (1–500) | `search` |
   | `node_info` | `path` | `NodeView`, with NodeId, flags and hard-link count |
   | `scan_volume` | `volume` (`C:`, or an image path), `profile=balanced` | scan job (D11); not read-only |
   | `scan_status` | `volume?` | scan job progress and outcome |

   - The server identifies itself as `adamite` with the crate version.
   - The `instructions` field (at most 1,000 characters) states the workflow and safety rules from D9.
6. D6. Snapshot discovery and selection.
   - Sources:
     - Each `--snapshot FILE`.
     - Every `*.adm` file directly inside `--snapshot-dir DIR`; symlinks are followed and subdirectories are not.
     - The directory defaults to the snapshot directory from [query-tui.md](query-tui.md) D4.
   - The server creates the default directory at startup if it is missing and treats it as empty when it cannot.
   - Discovery is re-run on every call and reads only file metadata.
   - `snapshots` lists, for each file:
     - its name, absolute path, byte length and modified time;
     - its scan time once loaded.
     Unreadable entries appear with their error and never fail the listing.
   - `snapshot` selection:
     - A path to an `.adm` file.
     - A volume name (`C:` or `C`), matching file stems ASCII-case-insensitively; two matches are `ambiguous_snapshot`.
     - Omitted: allowed only when exactly one snapshot is discovered.
   - The cache is an LRU of `Arc<ScanOutput>`, two entries by default (`--cache N`).
     - It is keyed by canonical path, and an entry is reloaded when the file length or mtime changes.
     - Only entries without running queries are evicted.
7. D7. Output.
   - The text header has the form `<snapshot file> · serial <hex> · scanned <RFC 3339 | unknown> (<age>) · sizes: on-disk allocation`.
   - Age is shown in the largest whole unit: `45s`, `12m`, `5h` or `3d`. It is `unknown` when the scan time is absent or in the future.
   - `get_tree` text uses two spaces of indent per depth with the columns `alloc  share  name`, sizes from `ByteSize` and share to one decimal place. A collapsed row reads `… N more (X)`.
   - A paging footer reads `showing A–B of N · next offset=B`; it is omitted on the last page.
   - With `format=json`, the tool returns one `adamite/1` object, as text and as `structuredContent`.
     - The MCP wrapper adds `snapshot.path`, `snapshot.age_seconds` and `snapshot.stale` (`--stale-after`, default 24 h). Both age fields are `null` when the age is unknown.
     - New reports carry `truncated` and `next_offset`.
   - Each response, text or JSON, is capped at 32 KiB. The cap drops whole items, never bytes, then sets `truncated` and `next_offset` and appends the footer. If a single item exceeds the cap, that item's path is shortened in the middle.
   - Returned paths start with `/` and use `/` separators. Synthetic directories keep their names, such as `/<NTFS metadata>`.
8. D8. Errors.
   - Malformed requests, unknown tools and parameters that fail to deserialize are protocol errors.
   - Every other failure is an `isError` result in both formats, written as two lines:
     - `error[<code>]: <what happened>`
     - `next: <exact step>`

   | Code | When | `next` |
   |---|---|---|
   | `no_snapshot` | Nothing was discovered, or `snapshot` names a drive or image with no discovered `.adm` | Ask the user, then call `scan_volume` for that volume. The manual fallback is `adamite scan C: -o "<absolute dir>\C.adm"` (`--force` when the target exists). |
   | `unsupported_source` | `snapshot` is a non-`.adm` file, or `scan_volume` names a drive on a non-Windows platform | The image scan command. |
   | `elevation_declined` | The user declined the UAC prompt | Ask whether to retry; never retry on your own. |
   | `scan_busy` | A scan job is already running | Call `scan_status`. |
   | `scan_failed` | The scan child exited with an error | The child's error text and exit code. |
   | `ambiguous_snapshot` | Volume name ambiguous, or `snapshot` omitted with several discovered | The candidates. |
   | `path_not_found` | Path does not resolve | The deepest resolved directory, to pass to `list_dir`. |
   | `ambiguous_path` | Path matches several entries | Candidate paths in `…/#id` form. |
   | `not_a_directory` | A directory tool got a file | Use `node_info`. |
   | `corrupt_snapshot` | The `.adm` file fails to decode | Rescan with `--force`. |
   | `permission_denied` | The file is not readable | Check access or rescan into the snapshot directory. |
   | `invalid_argument` | A parameter is out of range | The allowed range. |

   A failed call never stops the server.
9. D9. Plugin and Skill.
   - `.claude-plugin/marketplace.json`: `name: "adamite"`, `owner`, and `plugins: [{name: "adamite", source: "./plugins/adamite", description}]`.
   - `plugins/adamite/.claude-plugin/plugin.json`: `name`, `version` and `description`.
   - `plugins/adamite/.mcp.json`: `{"mcpServers":{"adamite":{"command":"adamite","args":["mcp"]}}}`. The binary is installed separately on PATH.
   - `plugins/adamite/skills/disk-usage/SKILL.md`:
     - Frontmatter: `name: disk-usage`. The `description` says what the Skill does and when to use it: disk full, large files or folders, cleanup, `.adm` snapshots and Adamite.
     - It links `references/cli.md`, the CLI fallback.
   - The Skill body:
     - Workflow: `snapshots`. If there is none or it is stale, tell the user a Windows UAC prompt will appear and ask before calling `scan_volume`. Then poll `scan_status`, `get_tree` at `/`, drill down with `path`, use `top_files` or `search` for candidates, and `node_info` before recommending.
     - A question → tool table.
     - Reading the numbers: alloc versus size; sparse, compressed and placeholder files; hard links; `<NTFS metadata>` and `<orphaned>`.
     - Hard safety rules: never delete, move or run cleanup commands; elevate only through `scan_volume` after the user agrees, and never retry a declined prompt; present candidates with size and age and let the user decide.
     - Fallback: when the adamite server's tools are absent, run `adamite … --json`.
   - Tools are named by their short names. Inside the plugin, Claude Code prefixes them.
   - Manual install has two steps:
     1. `claude mcp add --scope user adamite -- adamite mcp`.
     2. Copy `skills/disk-usage/` to `~/.claude/skills/`.
   - The plugin README documents both install paths and an allow rule for skipping permission prompts.
10. D10. Tests.
    - Core: path resolution keeps the current CLI cases. The summary is checked against its budget and collapse rules on deep and wide synthetic volumes. Ranking is checked against a full sort, including hits beyond the stored top 100, and covers extension totals under a subtree and search paging.
    - CLI: the existing tests stay unchanged, and new commands get text and JSON tests.
    - MCP:
      - In-process tests over `tokio::io::duplex` cover initialize, `tools/list` with annotations, every tool, every error code, truncation, discovery and cache reloads.
      - Scan jobs on an image, including progress and outcome from the status file.
      - One subprocess test runs `adamite mcp` over stdio on Linux and Windows. It checks the handshake, `get_tree` on a fixture `.adm`, that stdout carries only protocol messages, and that the process exits 0 on EOF.
11. D11. Scan jobs. This plan builds on `ElevatedProcess` and the status file from [elevation.md](elevation.md), which lands first.
    - `scan_volume` validates `volume` and builds the child arguments itself: the drive letter or the canonical image path, the profile, `-o <snapshot dir>/<stem>.adm --force` and `--status-file <snapshot dir>/.<stem>.status.jsonl`. No caller text reaches the elevated command line.
      - Images, and drives when the server is already elevated, scan in-process in `spawn_blocking` with the same job API.
      - The call returns after the UAC decision, with `running`, or with `elevation_declined`.
      - One job runs at a time.
    - `scan_status` reports `running` with phase, percentage, bytes and elapsed time, then `done` with the snapshot path (picked up by discovery), or `failed`.
    - An elevated child keeps running if the server exits, and still publishes its snapshot.
    - UAC consent is the security boundary. The server never elevates itself.

## Tasks

Each task ends with one reviewed commit, `deno task check` green and CI green. Each Codex run covers implementation and targeted verification and stays under 40 minutes; review and CI are separate.

- [ ] M1a — `Snapshot::resolve_path` in core; the CLI uses it (D3).
- [ ] M1b — Extract `adamite-access` (D2).
- [ ] M2 — `Snapshot::summarize`, the tree report and `adamite tree` (D3, D4).
- [ ] M4a — `adamite-mcp` discovery, selection and cache, without rmcp (D6).
- [ ] M4b — The rmcp server, the `adamite mcp` subcommand, `snapshots`, `snapshot_info` and `list_dir`, with the handshake test (D1, D5, D7, D8).
- [ ] M4c — The remaining tools, the output cap, every error code and the subprocess test (D5, D7, D8, D10).
- [ ] M4d — MCP `scan_volume` and `scan_status` (D5, D8, D11).
- [ ] M5 — The plugin and Skill (D9), plus ARCHITECTURE §1, §4 and §6 and the root, core, CLI and MCP READMEs.

## Acceptance criteria

- [ ] MCP Inspector (`npx @modelcontextprotocol/inspector adamite mcp`) lists ten tools; the eight query tools carry `readOnlyHint: true`.
- [ ] With no snapshot, `get_tree` returns `no_snapshot`. Asked "why is my C: drive full?" in Claude Code on Windows with the plugin installed, the agent asks before calling `scan_volume`. After the user agrees, a UAC prompt appears; accepting it lets `scan_status` show progress and then `done`, and the next `get_tree` answers from the new snapshot. Declining returns `elevation_declined`, and the agent does not retry or delete anything.
- [ ] `get_tree` on the bench synthetic volume, with default settings, starts with the header line, contains `… N more` rows and emits at most 60 rows.
- [ ] `list_dir` and `search` page through `next_offset` to `null`; an offset past the end returns an empty page.
- [ ] With `C.adm` and `D.adm` discovered, omitting `snapshot` returns `ambiguous_snapshot` and `snapshot: "D:"` succeeds. An ambiguous path returns `#id` candidates, and the retry succeeds.
- [ ] After `adamite scan <image> -o C.adm --force` over a cached snapshot, the next `snapshot_info` reports the new scan time without a restart. A truncated `.adm` returns `corrupt_snapshot`, and the next call succeeds.
- [ ] `/plugin marketplace add` and `/plugin install adamite@adamite` show the `disk-usage` Skill and the connected server. With the server removed, the Skill answers "largest files" and "what's in /Users" through `adamite top … --json` and `adamite ls … --json`.
- [ ] Existing CLI exact-text and JSON tests are unchanged; `deno task check` and CI pass on Linux and Windows.
