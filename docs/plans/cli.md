# Plan: adamite command-line and terminal UI

## Why

- The libraries can scan images and Windows volumes (`adamite-io`) and, after [snapshot-persist](snapshot-persist.md), save and load `.adm` files, but there is no program: the only runnable entry point is `adamite-io`'s development example, and CI publishes no binaries.
- ARCHITECTURE §4.1 needs a program that scans elevated once (`adamite scan C: -o C.adm`) and lets unelevated users and agents query the saved snapshot. §4.3 asks for compact text by default and JSON on request.
- Humans exploring disk usage want a WizTree-like interactive view; agents and scripts need non-interactive, machine-readable output.

## What Changes

Add two workspace crates. `crates/adamite-cli` produces the single `adamite` binary; `crates/adamite-tui` is a library holding the interactive terminal UI, which the binary launches:

- non-interactive commands `scan`, `info`, `top`, `ext`, `ls` with text output by default and `--json`;
- `browse`, an interactive terminal UI (ratatui, in `adamite-tui`) for navigating the size-sorted tree, top files, extensions and scan diagnostics;
- CI release builds uploaded as workflow artifacts on every run, and a tag-triggered workflow publishing those Windows x64 and Linux x64 artifacts to GitHub Releases.

`adamite-io` gains `ImageReader::context()` so image scans carry a scan timestamp. The `adamite-io` scan example is removed once the CLI replaces it.

## Scope

In scope: the crates above, `ImageReader::context()`, CI/release workflows, a Deno packaging script, gate/test updates, documentation including a new root `README.md`.

Out of scope: budgeted `tree` summaries, search, `top --under`, top directories and other dynamic queries (`add-core-query`); MCP; a daemon/service; Linux block devices; scan progress inside the TUI (the driver exposes no progress callback); rescans from the TUI; sorting toggles and mouse input in the TUI; code signing; package managers; publishing an actual release (a user-approved tag push, outside this plan).

Depends on [snapshot-persist](snapshot-persist.md) being complete.

Follow the project's Rust best practices and conventions: panic-free Clippy denies, checked arithmetic, no test-only fields, no `cfg(test)` staging or lint suppression to keep milestones green (dependencies and modules arrive with their first use), no bash/Python scripts (packaging in Deno), CI `run:` steps are cargo/deno one-liners (`uses:` steps are allowed).

## Impact

- New: `crates/adamite-cli/{Cargo.toml,README.md,src/**,tests/**}`, `crates/adamite-tui/{Cargo.toml,README.md,src/**}`, `.github/workflows/release.yml`, `scripts/package.ts`, `scripts/package_test.ts`, root `README.md`.
- `crates/adamite-io`: `ImageReader::context()`; `nt-time` moves to all targets; `examples/scan.rs` removed and README updated.
- `Cargo.lock`: `clap` (no `color`), `ratatui` (`default-features = false`, `crossterm`; a dependency of `adamite-tui` only), `serde`, `serde_json` (already locked via fixtures), `time` (`formatting`), and their dependencies. If `cargo deny check` reports a license outside the allow-list (e.g. `Zlib` from `foldhash`), add exactly that license to `deny.toml` in the milestone introducing the dependency.
- `.github/workflows/ci.yml`: `workflow_call` trigger, Deno setup and packaging in the `check` matrix, `actions/upload-artifact`, the Windows elevated CLI smoke step. `scripts/check.ts`, `scripts/check_test.ts`, `deno.json`/`deno.lock` (`@std/tar`, `package` and `release-check` tasks), `.gitignore` (`dist/`).
- ARCHITECTURE §1, §4.1, §4.3, §6, §7.

## Design decisions

1. D1. Crate. Package `adamite-cli`, `[[bin]] name = "adamite"`, `publish = false`, workspace lints plus core's panic-free Clippy denies; binary only (no `lib.rs`; integration tests run the binary), with a `//!` crate doc in `main.rs`. Private modules: `main.rs` (parse → run → `ExitCode`), `cli.rs` (clap derive), `source.rs` (detect/open/scan/load), `save.rs` (D7), `locate.rs` (D4), `command/{scan,info,top,ext,ls,browse}.rs`, `report.rs` (serde DTOs borrowing from the snapshot), `render/{text,json,units,time,escape}.rs`, `error.rs`. Dependencies: `clap` 4 (`default-features = false`, features `std, derive, help, usage, error-context, suggestions`), `serde`/`serde_json`, `nt-time` 0.15 (`default-features = false`, `std`) with `time` (`formatting`), `adamite-core`, `adamite-io`, `adamite-tui` (from C5), `snafu`. No tracing subscriber is installed.

   `adamite-tui`: library only, `publish = false`, the same lints and denies, `//!` crate doc in `lib.rs`. It depends on `adamite-core`, `ratatui` 0.30 (`default-features = false`, `crossterm`; crossterm types through ratatui's re-export) and `snafu`, and never on `adamite-cli`, `adamite-io` or clap. Public API (the only public items, at one path each): `adamite_tui::Browser::new(output: &ScanOutput, start: NodeId) -> Browser<'_>`, `Browser::run(self) -> Result<(), TuiError>` (owns terminal setup, event loop and restore), and `adamite_tui::TuiError` (snafu, `#[non_exhaustive]`, terminal I/O with an operation). Private modules `app.rs`, `view.rs`, `input.rs`, `escape.rs`. Loading sources, resolving the start PATH and mapping errors to exit codes stay in the CLI.

2. D2. Commands. `--json` and `--bytes` (raw byte counts in text) are accepted by the non-interactive commands only; `browse` rejects them (usage, exit 2). `adamite` without a subcommand prints help.
   - `adamite scan <SOURCE> [-o <FILE> [--force]]`: scan an image or drive. With `-o`, save `.adm` (D7) and print a one-line confirmation (JSON: `{"saved": path, "bytes": n}` in the envelope); without `-o`, print the `info` report. `--force` without `-o` and a snapshot SOURCE ("already a snapshot") are usage errors.
   - `adamite info <SOURCE>`: snapshot header plus volume, tree totals and `ScanStats` (text: non-zero diagnostics only; JSON: every counter).
   - `adamite top <SOURCE> [--n 20]`: precomputed top files; `--n` uses a clap range parser `1..=100` (out of range is a usage error naming the 100-entry index).
   - `adamite ext <SOURCE> [--n 20]`: extension statistics in stored order (`--n` `1..`).
   - `adamite ls <SOURCE> [PATH] [--offset 0] [--limit 50]`: children of a directory (PATH defaults to `/`) in size order; an offset past the end returns an empty list; a non-directory PATH is `NotADirectory` (exit 1).
   - `adamite browse <SOURCE> [PATH]`: the TUI (D6), starting at PATH (default `/`).

3. D3. Sources. One positional `SOURCE`, classified in this order:
   1. Drive: exactly two characters, an ASCII letter (either case) and `:`; `C:\` and longer strings are paths. Windows opens `RawVolumeReader::open_drive` and uses its `context()`; other targets fail with "raw volumes require Windows" (exit 2).
   2. Snapshot: a file whose first 8 bytes equal the `.adm` magic, or whose extension is `adm` (case-insensitive). Snapshot decoding failures (including short or damaged headers) exit 3 and never fall back to scanning.
   3. Image: any other file, opened with `ImageReader` and its `context()` (D9). Live sources print `scanning <source>…` to stderr before work (before the TUI takes the screen). Snapshot files: the length must fit `usize`; read into a `try_reserve_exact` buffer that is dropped right after decoding. Paths stay `PathBuf`; JSON `source` is the argument's `to_string_lossy`.

4. D4. Paths (`ls`, `browse`). Split on `/` and `\`; empty components are skipped (so the leading separator is optional); `.` and `..` have no special meaning. Per component, collect exact name matches among `children()`: one match succeeds; several return `AmbiguousPath` listing the candidates' NodeIds. With no exact match, compare `a.chars().flat_map(char::to_lowercase).eq(b.chars().flat_map(char::to_lowercase))` with the same uniqueness rule; none is `PathNotFound`. A `#<id>` component (e.g. `#1234`) selects a NodeId directly to resolve ambiguity. Synthetic directories (`<NTFS metadata>`, `<orphaned>`) are ordinary components. Output paths use core's `/` form; text and TUI output replace control characters in names with `\u{…}` escapes (names from `.adm` files are untrusted).

5. D5. Output.
   - Text: compact, aligned columns, binary units (`12.0 GiB`) unless `--bytes`; the first line names the source and scan time.
   - JSON: one object on stdout per non-interactive invocation: `{"schema": "adamite/1", "command", "snapshot": {"source", "scanned_at", "scanned_at_filetime", "volume_serial", "label"}, "units": {"size": "bytes"}, …}`. `scanned_at` is RFC 3339 UTC with seconds precision, or `null` when absent or outside the `time` range (`FileTime → UtcDateTime` conversion fails); `scanned_at_filetime` is a decimal string or `null`; `volume_serial` is a 16-digit hex string; byte counts are integers.
   - Command fields: `info` → `"volume": {bytes_per_sector, cluster_size, total_bytes, free_bytes}`, `"tree": {nodes, files, total_size, total_alloc}`, `"stats": {every ScanStats counter}`; `top` → `"items": [{path, size, alloc, mtime_filetime}]`; `ext` → `"items": [{extension, count, total_size, total_alloc}]`; `ls` → `"path", "total_children", "offset", "limit", "items": [{name, kind ("dir"|"file"), flags: [..], size, alloc, total_size, total_alloc, file_count}]`; `scan -o` → `"saved", "bytes"`. Names are snake_case and stable within `adamite/1`.
   - Errors are text on stderr only (also with `--json`); stdout then stays empty. A broken stdout pipe ends the command successfully.

6. D6. TUI (`browse`, implemented in `adamite-tui`).
   - Startup: the CLI's `command/browse.rs` requires stdout to be a terminal (else usage, exit 2) — checked before loading or scanning — then loads the source, resolves PATH (D4; a non-directory is `NotADirectory`) and calls `Browser::new(&output, id).run()`. Inside `run`: `ratatui::try_init`; on partial initialization failure attempt `try_restore`; after the event loop returns (success or error, including draw/read errors) always restore; ratatui's panic hook is kept. Terminal I/O errors are `TuiError`, wrapped by the CLI as `CliError::Terminal` (exit 1).
   - Tabs: Tree, Top files, Extensions, Info (volume + `ScanStats`).
   - Tree rows: name, allocated size, share of the parent's `total_alloc` as integer per-mille with a proportional bar (widths computed in `u128`, clamped to the bar width), logical size, file count. Parent `total_alloc == 0`: no percentage (`—`) and an empty bar. Directories marked `/`; synthetic directories dimmed; HARDLINK_DUP rows marked `=` with no bar (excluded from parent totals); reparse points marked `↪`. Empty directories show `(empty)`. Names are escaped (D4) and truncated by display width with `…`; the header path is truncated from the middle. Only visible rows are rendered.
   - Keys: `↑/↓`, `k/j`, `PgUp/PgDn`, `Home/End` move; `Enter`/`→`/`l` enter a directory; `←`/`h`/`Backspace` return to the parent, restoring its selection and scroll; `Tab`/`1–4` switch tabs; `b` toggles raw bytes; `?` help overlay; `q`/`Esc`/`Ctrl-C` quit. In Top files, `Enter` opens the file's parent in the Tree tab with the file selected. Key events: `Press` dispatched, `Repeat` dispatched for movement keys only, `Release` ignored (Windows reports both). `Resize` clamps selection and scroll. Below 40×8 cells, a "terminal too small" message replaces the views.
   - Structure: `input.rs` maps crossterm events to an internal `Key`/`Event`; `app.rs` is a pure state machine (`App::handle(Event) -> Action`, cursor/scroll per view, navigation stack); `view.rs` renders `&App` into a `Frame`; `Browser` in `lib.rs` owns the terminal and loop. `escape.rs` holds the crate's own control-character escaping (D4), independent of the CLI's `render::escape`.

7. D7. Saving. A same-directory temporary file named `.<file>.<pid>-<n>.tmp`, created with `create_new` and up to 16 retries on collision; write, `sync_all`, close. Without `--force`, publish with `std::fs::hard_link(temp, dest)` (fails if `dest` exists, including when created concurrently) and then remove the temporary file; if the filesystem does not support hard links, fail with `UnsupportedNoClobber` suggesting `--force`. With `--force`, publish with `std::fs::rename` (replaces the destination; never delete it first). Remove the temporary file on every failure. Reject a destination that canonicalizes to the scanned image. The encoding buffer is dropped after writing.

8. D8. Errors and exit codes. `CliError` (snafu) wraps `VolumeError`, `PersistError`, I/O with paths and operations, `SnapshotError`, terminal errors, `PathNotFound`, `AmbiguousPath`, `NotADirectory`, `OutputExists`, `UnsupportedNoClobber`, `SameAsSource`, usage problems. Errors print `error: …` then `caused by: …` lines. Exit codes, first matching rule wins:

   | Code | Condition                                                                                                                                                                    |
   | ---- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
   | 0    | success, including clap `--help`/`--version` and broken pipes                                                                                                                |
   | 2    | clap usage errors and the D2/D3/D6 usage rules                                                                                                                               |
   | 3    | any `PersistError`                                                                                                                                                           |
   | 4    | `VolumeError::AccessDenied`, or any I/O error of kind `PermissionDenied` (opening sources, reading snapshots, writing output); drives add a hint to use an elevated terminal |
   | 1    | everything else (I/O, scan, allocation, path and output errors)                                                                                                              |

9. D9. Image timestamp. `adamite_io::reader::ImageReader::context(&self) -> Result<ScanContext, VolumeError>` returns the current FILETIME (`nt_time::FileTime::try_from(SystemTime::now())`, failure → `Overflow`) with no label or free bytes. `nt-time` moves to the all-target dependency table.

10. D10. Tests.
    - Integration (`tests/commands.rs`): fixture images written to `CARGO_TARGET_TMPDIR` under per-test names; `Command::new(env!("CARGO_BIN_EXE_adamite"))`. JSON is parsed with `serde_json::Value` and compared structurally with an in-process `SyncDriver` scan of the same image (node counts, root totals, `top`/`ext`/`ls` contents and order, every `info` stats counter). Exact text expectations for `basic-4k` exclude the first (time-bearing) line.
    - Round trip and failures: `scan img -o a.adm`, then every command's JSON from `a.adm` equals that from `img` except `snapshot.source` and the scan time fields; existing-output refusal, `--force`, same-as-source rejection; `.adm` truncated inside the magic, with each magic byte flipped, with a body bit flipped → exit 3 without "panicked" on stderr; missing path, ambiguous and `#id` paths, `ls` on a file, offset past end, bad flags, `browse --json`, `scan C:` on Linux, `scan` of a snapshot. Exit 4 is tested only when a `chmod 000` file is actually unreadable (skipped otherwise, e.g. as root).
    - Time rendering: FILETIME 0, an ordinary value and `u64::MAX` (→ `null`).
    - TUI (unit tests inside `adamite-tui`, which keeps `App` private; `adamite-fixtures` as a dev-dependency): `App` driven with `Event` sequences and `view` rendered into `ratatui::backend::TestBackend`, on a snapshot from an in-process `basic-4k` scan plus small synthetic cases; assertions on row substrings, not whole buffers. Cover entering/leaving directories with selection restore, empty directories, zero-allocation parents (`frag-512` `/growth`), hard-link duplicates, long names, tab switching, top-file jump, byte toggle, resize and the too-small message.
    - Windows (`tests/raw_volume.rs`, `#![cfg(windows)]`, `#[ignore]`): `scan C: -o <tmp>/C.adm`, then `info --json` and `top --n 5 --json` on the file in new processes; assert a timestamp and non-empty results.

11. D11. Packaging. `scripts/package.ts` (`import.meta.main` guard): `deno task package <Linux|Windows>` reads the `adamite-cli` version from its manifest and writes to `dist/`: Windows `adamite-<version>-x86_64-pc-windows-msvc.exe`; Linux `adamite-<version>-x86_64-unknown-linux-gnu.tar.gz` (`@std/tar` pinned to an exact version in `deno.json`/`deno.lock`, binary entry mode `0o755`, gzip via `CompressionStream`); each with a `.sha256` (WebCrypto). `deno task release-check <tag>` fails unless `tag == "v" + version`. `scripts/package_test.ts` uses a temporary test tree (no real release binary) and covers names, tar mode, checksums, unknown OS, missing binary and tag mismatch, with scoped read/write permissions. `dist/` is added to `.gitignore`.

12. D12. CI artifacts. `ci.yml` gains `on: workflow_call`. In the `check` matrix, after tests: `denoland/setup-deno` (pinned version, as the deno job), `cargo build --release -p adamite-cli --locked`, `deno task package ${{ runner.os }}`, `actions/upload-artifact` (`name: adamite-${{ runner.os }}`, `path: dist/`, `if-no-files-found: error`). Windows also runs `cargo test -p adamite-cli --test raw_volume --locked -- --ignored`.

13. D13. Release. `.github/workflows/release.yml` on `push: tags: ['v*']`, no `concurrency` key: job `ci` (`uses: ./.github/workflows/ci.yml`, `permissions: contents: read`); job `publish` (`needs: ci`, `permissions: contents: write`): checkout, setup-deno, `deno task release-check ${{ github.ref_name }}`, `actions/download-artifact` for `adamite-Linux` and `adamite-Windows` into `dist/` (merged), then `softprops/action-gh-release` (pinned major, `files: dist/*`, `fail_on_unmatched_files: true`). Nothing is rebuilt; the published files are exactly the four produced by the called CI run. `ci.yml` itself is not triggered by tags.

14. D14. Gate and workflow assertions. Windows Clippy becomes `-p adamite-io -p adamite-cli` in C2 and adds `-p adamite-tui` in C5. `check.ts` adds the `package_test.ts` step; the CI deno job runs the identical command (existing mirror assertion). `check_test.ts`: model ordinary jobs (`steps`) and reusable-workflow jobs (job-level `uses`) separately; extend `WorkflowStep` with `uses`; apply the one-liner rule to every ordinary job step in every workflow file; assert the new CI steps (setup-deno in each job running `deno task package`, release build, package, upload, Windows CLI smoke), `workflow_call`, and `release.yml` (tag filter, no concurrency, `ci` reuse, permissions split, publish steps and artifact names). Check-script tests read `.github/workflows` as a directory; update their permission strings together in `check.ts`, `check_test.ts` (including the scoped-permission test) and `ci.yml`.

15. D15. Documentation. New `crates/adamite-cli/README.md` (commands, sources, JSON schema, exit codes, Windows elevation workflow, pointer to the TUI) and `crates/adamite-tui/README.md` (API, tabs, keys, rendering rules); ARCHITECTURE's crate table lists both. Root `README.md`: what Adamite is, downloads (Releases and CI artifacts), quick start (`adamite scan C: -o C.adm` elevated, then `adamite browse C.adm`), building from source. ARCHITECTURE: CLI/TUI implemented in §1, §4.1, §4.3 (CLI mirrors a subset; `tree`/search pending `add-core-query`), §6, §7 (CLI installs no subscriber). The `adamite-io` README drops the example section; the example file is deleted.

## Tasks

Each milestone ends with one reviewed commit and `deno task check` green. A milestone may use more than one bounded Codex run (< 40 minutes each) before that commit.

### C1 — Image scan timestamp

- [x] Add `ImageReader::context()` per D9 with tests (timestamp present, no label or free bytes); move `nt-time` to all targets.

### C2 — CLI skeleton, sources and info

- [ ] Add the crate per D1 with `cli.rs`, `source.rs`, `error.rs`, `report.rs` and `render/*` for `info` and `scan` without `-o`, D3 source detection, D5 envelope and D8 exit codes. Extend Windows Clippy (D14). Integration tests for info on images and snapshots, JSON structure, time rendering and source/usage failures.

### C3 — Saving

- [ ] Add `save.rs` and `scan -o` per D7 with round-trip, refusal, `--force`, same-as-source and corrupt-snapshot tests.

### C4 — top, ext, ls and paths

- [ ] Add `command/{top,ext,ls}.rs` and `locate.rs` per D2, D4, D5 with structural, exact-text and path tests.

### C5 — TUI navigation

- [ ] Add the `adamite-tui` crate per D1/D6 with `Browser`, `TuiError`, the event loop, `App` navigation and the Tree tab; the `ratatui` dependency and any `deny.toml` license addition; Windows Clippy `-p adamite-tui` (D14). Add `browse` to the CLI (`command/browse.rs`, `CliError::Terminal`). `App`/`view` tests for the Tree tab and CLI tests for `browse` usage failures.

### C6 — TUI tabs and edge cases

- [ ] Add the Top files, Extensions and Info tabs, help overlay, top-file jump, byte toggle, resize and too-small handling; their tests.

### C7 — Packaging script

- [ ] Add `scripts/package.ts`, `package_test.ts`, `deno.json` tasks, the pinned `@std/tar` with `deno.lock`, `.gitignore` entry, and the gate step with its mirror and assertions (D11, D14).

### C8 — CI artifacts and Windows CLI smoke

- [ ] Add the D12 steps, `workflow_call`, `tests/raw_volume.rs` and the D14 `ci.yml` assertions, including the workflow-model changes in `check_test.ts`.

### C9 — Release workflow and documentation

- [ ] Add `release.yml` per D13 with its D14 assertions; documentation per D15; delete the `adamite-io` example.

## Acceptance criteria

- [ ] `adamite scan <fixture image> -o f.adm`, then `info/top/ext/ls --json` on `f.adm` match the image-sourced output except source and scan time; exit codes follow D8.
- [ ] TUI tests cover navigation, tabs, edge cases and rendering per D10.
- [ ] After the C8 commit, both CI `check` jobs upload `adamite-Linux`/`adamite-Windows` artifacts containing the binary and checksum, and `check (windows-latest)` passes the elevated CLI smoke.
- [ ] `check_test.ts` asserts the `release.yml` structure; publication itself is verified on the first user-approved tag and is outside this plan's acceptance.
- [ ] `deno task check` (Windows Clippy for io and cli, package tests) and `cargo deny check` pass; READMEs and ARCHITECTURE match the implementation.
