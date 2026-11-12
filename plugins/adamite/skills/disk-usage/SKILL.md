---
name: disk-usage
description: Investigate NTFS disk usage with Adamite and saved .adm snapshots. Use when a disk is full, locating large files or folders, reviewing cleanup candidates, or checking snapshot age.
---

# Disk usage

Use Adamite to explain where allocated disk space went and present candidates for the user to review. Tool names below are short names; Claude Code adds the plugin or server prefix.

## Workflow

1. Call `snapshots` to discover saved `.adm` files. Select the relevant absolute snapshot path or volume letter (`C:` or `C`); supply `snapshot` on later queries when several files exist. The catalog reads file metadata; scan time is known only for loaded snapshots. Use `snapshot_info` to load a selection and check its scan time and age when unknown.
2. If no snapshot exists or it is stale, ask the user before `scan_volume`. For an unelevated Windows drive scan, tell them a Windows UAC prompt will appear. Explain that scanning replaces `<snapshot-dir>/<stem>.adm`. If they prefer the existing snapshot, report its age with the results.
3. After consent, call `scan_volume` with `volume: "C:"` (Windows) or an existing NTFS volume image path. `profile` defaults to `balanced`; alternatives are `background` and `max`. Poll `scan_status` until `done` or an error. Pass the returned `snapshot.path` to subsequent queries. One scan can run at a time; status retains the last outcome.
4. Call `get_tree` with `path: "/"`. Drill into a large folder by changing `path`, then use `list_dir` for its children. Paths are volume-relative, use `/` separators and may contain direct-child identifiers such as `/Users/#1234` to resolve ambiguity.
5. Use `top_files` or `search` to find candidates. Scope them with `under` when investigating one folder. Use `ext_breakdown` to explain category or extension totals.
6. Call `node_info` for each candidate before recommending it. Present its path, allocation, logical size, modification age when available, relevant flags and snapshot age. Let the user decide what to do.

## Questions and tools

| Question | Tool and useful parameters |
| --- | --- |
| Why is C: full? | `get_tree` with `snapshot: "C:"`, `path: "/"`; drill into the largest folders. |
| What are the largest files? | `top_files`, optionally `under: "/Users"`. |
| Are videos taking space? | `top_files` with `kind: "video"`; `ext_breakdown` with `by: "kind"` for totals. |
| Find `*.iso` | `search` with `pattern: "*.iso"`. |
| What is in this folder? | `list_dir` with `path`; continue with `next_offset`. |
| What is this file? | `node_info` with `path`. |
| Is my snapshot current? | `snapshots`, then `snapshot_info` if scan time is unknown. |

## Parameters and limits

The eight snapshot query tools accept optional `snapshot` and `format: "text" | "json"` (default `text`). Omit `snapshot` only when exactly one file is discovered, except that `snapshots` without a selection lists the catalog. JSON returns an `adamite/1` object as text and matching structured content.

| Tool | Other parameters |
| --- | --- |
| `snapshots`, `snapshot_info` | None. |
| `get_tree` | `path="/"`, `budget=60` (1–400 rows, including root and collapsed rows), `min_share_percent=0.5` (finite percentage 0–100, rounded down to basis points). |
| `list_dir` | `path="/"`, `offset=0`, `limit=50` (1–500). |
| `top_files` | `n=20` (1–100), optional `under`, and either `ext` (array of extensions) or `kind`, mutually exclusive. |
| `ext_breakdown` | `n=20` (1–200), optional `under`, `by="extension"` or `"kind"`. |
| `search` | Required `pattern` (at most 256 bytes), optional `under`, `offset=0`, `limit=50` (1–500). |
| `node_info` | Required `path` to a file or directory. |
| `scan_volume` | Required `volume`, `profile="balanced"` (`background`, `balanced`, `max`), `format="text"`. |
| `scan_status` | Optional `volume` matching the current or last scan; `format="text"`. Omit `volume` to check `idle` before any scan. |

`under` defaults to the whole volume (`/`). Kinds are `video`, `audio`, `image`, `archive`, `disk-image`, `document`, `code`, `executable`, `other`; they are inferred from extensions. For extensions, use e.g. `ext: ["mp4", ".mkv"]`; an empty string matches files without an extension. Search is case-insensitive: plain text is a substring, while `*`, `?`, `[abc]` and `{a,b}` form a whole-name glob, with backslash escaping.

Responses are capped at 32 KiB, including the MCP result wrapper and duplicated structured JSON. Check `truncated` and `next_offset`. `list_dir` and `search` accept the next offset; offsets beyond the end return an empty page. For a truncated tree or ranking, narrow `path` or `under`; other tools have no offset parameter. Oversized paths may be shortened, so resolve the full path with a narrower query before inspecting a candidate.

## Reading the numbers

- `alloc` measures on-disk allocation; `size` measures logical unnamed-stream bytes. `total_alloc` and `total_size` include descendants. Tree rows already contain subtree totals. Allocation includes non-resident alternate streams, while their logical bytes are recorded separately.
- Sparse holes and compressed data can make allocation much smaller than logical size; preallocation can make it larger. Cloud placeholders may have large logical sizes with little local allocation. Read sparse, compressed and reparse flags with `node_info`; reparse targets are not followed.
- Hard links share data. Totals and file rankings count allocation once at the primary link; secondary links carry `hardlink_dup`. Listings and search can show every link. Inspect the hard-link count before recommending a candidate.
- `<NTFS metadata>` groups filesystem records. `<orphaned>` groups entries whose parent could not be resolved, including repaired cycles. Explain these totals as such when they appear.
- Results describe the saved scan. The default stale threshold is 86,400 seconds (24 hours), configurable with `adamite mcp --stale-after`. JSON uses `snapshot.age_seconds` and `snapshot.stale`; unknown or future scan times make both null. The catalog's per-file fields carry age instead of its top-level snapshot. File modification age and snapshot age are separate facts; report unavailable times as unknown.

## Safety rules

- Never delete, move or run cleanup commands.
- Elevate only through `scan_volume` after the user agrees.
- Never retry a declined UAC prompt on your own. Ask whether the user wants to retry, and wait for agreement.
- Present candidates with size and age and let the user decide.

## Errors

Tool failures set `isError` and return `error[code]: ...` followed by `next: ...`. JSON wraps those same two lines in its `error` field. Read the code and follow the specific `next:` step within the consent rules above.

| Code | Action |
| --- | --- |
| `no_snapshot` | Ask about scanning the requested volume, then use `scan_volume` after consent. |
| `unsupported_source` | Query an existing `.adm`; use an NTFS image on non-Windows. Follow the source-specific next step. |
| `elevation_declined` | Stop and ask whether to retry; wait for agreement. |
| `scan_busy` | Poll `scan_status`. |
| `scan_failed` | Show the scan error and child exit code when supplied; inspect the cause before proposing another scan. |
| `ambiguous_snapshot` | Select one of the returned absolute paths, asking the user if the intended volume is unclear. |
| `path_not_found` | Call `list_dir` with the deepest resolved directory from `next:`. |
| `ambiguous_path` | Retry with one of the returned paths containing `/#id`. |
| `not_a_directory` | Call `node_info` on that file. |
| `corrupt_snapshot` | Propose rescanning the original source and replacing the snapshot after consent. |
| `permission_denied` | Check file access or propose rescanning into the snapshot directory. |
| `invalid_argument` | Correct the parameter to the range or values in `next:`. |
| `internal_error` | Inspect the reported path and I/O failure, correct the cause, then retry. |

Malformed requests, unknown tools and parameters that fail to deserialize are protocol errors; correct the request before retrying. A failed tool call leaves the server running.

## CLI fallback

When the Adamite server's tools are absent, read [references/cli.md](references/cli.md) and use `adamite … --json` on saved snapshots. That reference gives exact query flags and user-run scan commands. Keep the same safety and consent rules.
