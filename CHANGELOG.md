# Changelog

## 0.1.0-beta.1

First beta of Adamite, a fast NTFS disk-usage scanner that reads the Master File Table directly and saves the result as a compact `.adm` snapshot.

### Scanning

- `adamite scan C:` reads a live NTFS volume on Windows; any NTFS image works on every platform.
- Drive scans from an unelevated terminal request administrator access through one UAC prompt (`--no-elevate` opts out).
- `--profile background|balanced|max`, `--max-read-rate` and `--threads` bound I/O and CPU use.
- A progress bar shows the scan phase, records and bytes read.
- `-o FILE.adm` saves a snapshot; queries on a snapshot never rescan.

### Queries

- `info`, `ls`, `tree` (budgeted summary with `--budget` and `--min-share`), `top`, `ext` and `search` work on drives, images and snapshots, in text or `--json` (`adamite/1`).
- `top` and `ext` scope to a directory with `--under`; `top` filters by `--ext` or `--kind` (video, audio, image, archive, disk-image, document, code, executable, other); `ext --by kind` groups by category.
- `search` matches names case-insensitively by substring or glob, with paging.

### Terminal UI

- `adamite` with no arguments opens a home screen listing NTFS drives and saved snapshots; it scans or reopens them, with progress.
- Saved snapshots live in `snapshots\` next to `adamite.exe` on Windows and in the XDG data directory elsewhere.
- Tabs: Tree, Top files, Extensions, Info and Search, with directory scope (`u`), filters (`f`), kind grouping (`g`), name search (`/`) and jump-to-tree.

### MCP server and Claude Code plugin

- `adamite mcp` serves ten tools over stdio: `snapshots`, `snapshot_info`, `get_tree`, `list_dir`, `top_files`, `ext_breakdown`, `search`, `node_info`, `scan_volume` and `scan_status`.
- Query tools are read-only. `scan_volume` asks for UAC consent and publishes the snapshot for later queries.
- Responses are capped at 32 KiB, and failures return an `error[code]` line with an exact `next:` step.
- This repository is a Claude Code plugin marketplace: `/plugin marketplace add 4o3F/Adamite`, then `/plugin install adamite@adamite`. It adds the MCP server and the `disk-usage` Skill, which asks before scanning and never deletes files.
- Other MCP clients can register `adamite mcp` directly.

### Downloads

- `adamite-0.1.0-beta.1-x86_64-pc-windows-msvc.exe` and the Linux `x86_64-unknown-linux-gnu` archive, each with a SHA-256 checksum.
- Put `adamite` on `PATH` for the MCP server and plugin. On Windows, keep it in a writable folder (not `Program Files`) so `snapshots\` can be created.

### Known limitations

- Beta: the `.adm` format and JSON fields may still change before 0.1.0.
- Live drive scans are Windows-only; other platforms scan NTFS images.
- The UAC flow through the MCP server and the plugin install are still awaiting manual acceptance on Windows.
- A scan cannot be cancelled from MCP, and an elevated scan keeps running if its parent exits.
