# Plan: filtered queries and a TUI home screen

## Why

- Snapshots only answer through their stored global indexes: the top 100 files and the volume-wide extension totals. Nothing filters by extension or file type, scopes to a directory, or searches by name.
- The TUI can browse only the source given on the command line, and `adamite` with no arguments prints help. It should open the TUI and let the user pick a drive or snapshot, like `claude` or `codex`.
- [mcp.md](mcp.md) needs the same queries; this plan lands them first.

## What Changes

- Core gains file filters (extension list or file kind), subtree file ranking, subtree extension and kind totals, and name search.
- The CLI gains `top --under/--ext/--kind`, `ext --under/--by` and `adamite search`.
- The TUI scopes Top files and Extensions to a directory, filters them, groups by kind, and gains a Search tab.
- `adamite` with no arguments on a terminal opens a TUI home screen that lists drives and saved snapshots, scans with progress (through UAC when needed), and then browses.

## Scope

In scope: the core queries, CLI flags and `search`, the TUI changes, the home screen, drive listing and the snapshot directory in `adamite-io`, tests, the core, CLI and TUI READMEs, and ARCHITECTURE §4.

Out of scope:
- `tree`/`summarize`, `adamite-access` and everything MCP ([mcp.md](mcp.md)).
- Regex search, content search, date and size-range filters.
- Cancelling a scan, and deleting or opening files from the TUI.
- User-defined file kinds.

## Design decisions

1. D1. Core queries. Each is a method on `Snapshot`. They stay sans-io and panic-free, allocate fallibly, traverse iteratively and fail with snafu `QueryError`.

   | Method | Behavior |
   |---|---|
   | `rank_files(under, &FileFilter, n)` | Bounded min-heap over the subtree, ordered by the `rules.rs` comparison. With `ROOT`, no filter and `n ≤ 100`, it equals the `top_files()` prefix. |
   | `extension_stats_under(under)` | Merges the subtree's files by extension key, sorted like `ext_stats()`. `ROOT` returns `ext_stats()`. |
   | `kind_stats(under)` | Folds `extension_stats_under` by `FileKind`, sorted by allocation descending. |
   | `search(under, &NamePattern, offset, limit)` | Pre-order traversal in child order. The offset counts matches; the page carries `total_matches`. |

   - `FileFilter` is `All`, `Extensions(list)` or `Kind(FileKind)`.
     - Each extension drops one leading dot and folds with the `rules.rs` key. `""` matches names without an extension, and a list matches any of its entries.
   - `FileKind` is `Video`, `Audio`, `Image`, `Archive`, `DiskImage`, `Document`, `Code`, `Executable` or `Other`.
     - It comes from one sorted static extension table in core, for example `mp4`→Video, `flac`→Audio, `heic`→Image, `7z`→Archive, `iso`/`vhdx`→DiskImage, `pdf`→Document, `rs`→Code and `dll`→Executable.
     - Any other extension maps to `Other`. The README lists the table.
   - `NamePattern::glob(&str)`:
     - Uses `globset` with `case_insensitive(true)`, `literal_separator(false)` and `backslash_escape(true)`, and matches the whole name.
     - A pattern without `*?[{` becomes `*text*`, so `steam` finds `SteamLibrary`.
     - Patterns over 256 bytes are rejected.
   - `HARDLINK_DUP` nodes stay out of rankings and totals. Search still lists them, with their flag.
2. D2. CLI.
   - `top [--under PATH] [--ext EXT[,EXT…] | --kind KIND] [--n 20]`
     - `--n` keeps its 1–100 range.
     - Without filters, `top` reads the stored index and its output is byte-identical to today's.
   - `ext [--under PATH] [--by extension|kind] [--n 20]`.
   - `search <SOURCE> <PATTERN> [--under PATH] [--offset 0] [--limit 50]`, with a limit of 1–500. The text footer shows `showing A–B of N · next offset=B`.
   - `--under` resolves through the existing `Locator` and must name a directory.
   - Every command supports text and `--json` (`adamite/1`). `search` JSON carries `total_matches` and `next_offset`. Existing reports gain no fields.
3. D3. TUI queries.
   - Scope: Top files, Extensions and Search query a scope directory, which starts at `/`.
     - `u` toggles the scope between the Tree's current directory and `/`.
     - The tab bar shows `under <path>` when the scope is not `/`.
   - Top files: `f` opens an input line that takes `mp4,mkv`, `kind:video` or empty to clear. Invalid input shows an error in the status line and keeps the old filter.
   - Extensions: `k` toggles between extension and kind grouping. Enter on a row opens Top files filtered by it, with the same scope.
   - Search: a new tab 5. `/` opens its input from any tab, and Enter runs the search. It shows the first 1000 matches, plus `N more, refine the pattern` when there are more.
   - Enter on a Top files or Search row jumps to the file in Tree. The existing `jump_to_file` becomes a general `jump_to(node)`.
   - Input lines use the `tui-input` crate when it supports ratatui 0.30. Otherwise `input.rs` gains a minimal line editor: insert, backspace, left/right, Enter, Esc.
   - Queries run synchronously when the input is confirmed. Each must take under 200 ms in release on the bench synthetic volume.
4. D4. Home screen.
   - `browse [SOURCE [PATH]]`: without a source it opens the home screen.
   - `adamite` with no subcommand runs `browse` without a source when stdout is a terminal. Otherwise it prints the long help as today, and `--help` is unchanged.
   - The snapshot directory is `etcetera::choose_base_strategy()` data dir + `adamite/snapshots`, owned by `adamite-io`. It is created on first save. [mcp.md](mcp.md) D6 discovers the same directory.
   - The home screen rows:
     - On Windows, one row per NTFS volume. `adamite-io` adds `Volume::list()` (`GetLogicalDriveStringsW`, `GetDriveTypeW` fixed or removable, `GetVolumeInformationW` file system `NTFS`). Each row shows the letter, label and capacity, plus the age of `<dir>/<LETTER>.adm` when that file exists.
     - Saved `*.adm` files in the directory that no drive row claimed, with their modified time.
     - `Open file…`: an input line for an image or `.adm` path.
     - On Linux, only the last two exist.
   - Actions:
     - Enter on a drive opens its snapshot, or scans when there is none. `r` rescans.
     - Enter on a snapshot loads it.
     - A drive scan saves to `<dir>/<LETTER>.adm` (replace). An image scan is not saved.
   - Scanning:
     - A progress screen shows the phase, a gauge over records/planned, bytes and rate.
     - Unelevated drive scans use the existing elevated child: `-o <dir>/<LETTER>.adm --force --status-file …`. A declined prompt returns to home with `elevation was declined`.
     - Elevated drive scans and image scans run in-process on a worker thread.
     - On `done`, the TUI loads the result and opens Tree.
   - In a browser opened from home, `o` returns to home; `q` always quits.
     - Quitting during an elevated scan lets the child finish and publish its snapshot.
     - Quitting during an in-process scan ends it.
   - Layering: `adamite-tui` stays free of I/O.
     - It defines `trait Sources` with `entries()` and `start(&Entry) -> Box<dyn ScanJob>`, and `ScanJob::poll() -> JobState { Running(DriverProgress), Done(ScanOutput), Failed(String) }`.
     - The CLI implements both with `Source` and `ElevatedScan`.
     - `ElevatedScan::monitor` becomes a poll-based job, and both the CLI progress view and the TUI drive it, so there is one loop.
5. D5. Tests.
   - Core:
     - `rank_files` against a full sort, including hits beyond the stored 100.
     - The `ROOT`/`All` prefix property.
     - `extension_stats_under(ROOT) == ext_stats()`, and kind totals that sum to the extension totals.
     - Search paging, glob options and the substring rule.
   - CLI: the existing tests are unchanged. New flags and `search` get text and JSON tests, and the no-argument non-terminal run still prints help.
   - TUI: `App` tests with `TestBackend` for scope, filter input, kind grouping, Search, jumps, and the home screen with a fake `Sources` whose job goes `Running` → `Done` and → `Failed`.
   - io: on Windows CI, `Volume::list()` contains `C:`.

## Tasks

Each task ends with one reviewed commit, `deno task check` green and CI green. Each Codex run stays under 40 minutes.

- [ ] Q1 — `FileKind`, `FileFilter`, `rank_files`, `extension_stats_under`, `kind_stats` (D1).
- [ ] Q2 — `NamePattern` and `search` (D1).
- [ ] Q3 — CLI `top`/`ext` flags and `adamite search`, with the core and CLI READMEs (D2).
- [ ] Q4 — TUI scope, filter, kind grouping, Search tab and jumps (D3).
- [ ] Q5 — `Volume::list`, the snapshot directory and the poll-based elevated job; CLI behaviour unchanged (D4).
- [ ] Q6 — Home screen, in-TUI scanning, `adamite`/`browse` without a source, the TUI README and ARCHITECTURE §4 (D4).

## Acceptance criteria

- [ ] `adamite top C.adm --ext mp4,mkv --under /Users` and `--kind video` list files beyond the stored top 100. `adamite ext C.adm --by kind` shows kind totals that add up to the volume's file allocation.
- [ ] `adamite search C.adm '*.iso'` and `adamite search C.adm steam` page through `next offset` to the end.
- [ ] In the TUI, `u`, `f kind:video`, `k` and `/` work as described, Enter jumps to the file in Tree, and each query answers in under 200 ms on the bench synthetic volume.
- [ ] In an unelevated Windows terminal, `adamite` opens the home screen with the NTFS drives. Enter on `C:` shows the UAC prompt, then the progress screen, then Tree; next time `C:` opens the saved snapshot, and `r` rescans.
- [ ] `adamite | cat` and `adamite --help` print help as today; existing CLI exact-text and JSON tests are unchanged; `deno task check` and CI pass on Linux and Windows.
