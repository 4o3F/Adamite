# adamite-tui

`adamite-tui` provides a read-only terminal browser and a source launcher. The [CLI](../adamite-cli/README.md) loads sources, resolves paths and launches the browser with `adamite browse <SOURCE> [PATH]`. The library depends on core and shared formatting; source I/O stays with the caller.

## Public API

- `Browser::new(output: &ScanOutput, start: NodeId) -> Browser<'_>` borrows a completed scan and selects the starting directory. Invalid or file identifiers fall back to the root.
- `Browser::run(self) -> Result<(), TuiError>` initializes the terminal, draws views and reads input until quit, then restores the terminal. Restoration is attempted after partial initialization and after every event-loop result. A loop error takes precedence over a restoration error; ratatui's restoration panic hook is retained.
- `TuiError` is a non-exhaustive error enum. `TuiError::Io { operation, source }` identifies the failed terminal operation and preserves the underlying `std::io::Error`.
- `Launcher::new(sources: S) -> Launcher<S>` accepts a `Sources` implementation and lists its initial entries. `run(self) -> Result<(), TuiError>` uses the same terminal initialization and restoration as Browser, shows Home, polls jobs and browses completed scans.
- `Sources::entries(&mut self) -> Vec<Entry>` supplies ordered rows. `start(&mut self, entry: &Entry, rescan: bool) -> Box<dyn ScanJob>` opens the row, scanning a drive when needed or when `rescan` is true. `open_path(&mut self, path: &str) -> Box<dyn ScanJob>` opens a typed image or snapshot path. Providers own filesystem access, elevation and worker behavior; starting and polling jobs should return promptly.
- `Entry` is `Drive { letter: char, label: String, capacity: u64, snapshot: Option<SnapshotInfo> }` or `Snapshot(SnapshotInfo)`. `SnapshotInfo` has public `name: String`, `path: String`, `modified: Option<SystemTime>` and `error: Option<String>` fields. The path string identifies the source to the provider and can preserve a display representation of a native path.
- `ScanJob::poll(&mut self) -> JobState` returns `Running { label: String, progress: Option<JobProgress> }`, `Done(ScanOutput)` or `Failed(String)`. `JobProgress` has public `phase: String`, `records: u64`, `planned: Option<u64>` and `bytes: u64` fields. An absent progress report retains displayed counters. Snapshot loads use this same interface and may finish on the first poll.

## Home and progress

Home shows drive letters, labels and capacities, with the saved snapshot's age or `not scanned`. Other snapshot rows show their name and modification age, or their listing error. The fixed last row is `Open file…`; an empty list also shows a hint. Ages use the shared, pure `adamite_format::Age::new(modified, now)` formatter: seconds, minutes, hours or days followed by `ago`, and `unknown` for missing or future timestamps.

Use `↑` / `k`, `↓` / `j`, `Home`, `End`, `PgUp` and `PgDn` to select a row. Enter opens a snapshot or scans a drive that has none. On a drive, `r` requests a rescan. Enter on `Open file…` opens the same one-line editor used by the browser; Enter submits the path and Esc cancels. `?` opens Home help; `q` or `Ctrl-C` quits outside input lines.

The progress screen shows the job title, phase, a records gauge when the total is known, bytes read and the byte rate since the preceding report. Unknown totals display text instead of a gauge. While a job runs, the event loop polls input with a 100 ms timeout and redraws without waiting for a key. Esc has no effect; `q` quits and drops the job. The provider controls whether that drop stops the work.

On completion, Launcher owns the moved `ScanOutput` while the browser App borrows it. In this browser, `o` drops the result and returns to a refreshed Home, and `q` quits. A failed job returns to refreshed Home with its message on the status line. A standalone Browser retains its existing controls.

## Tabs and controls

The five tabs are Tree, Top files, Extensions, Info and Search. Each keeps its own selection and scroll position. Tree lists children by subtree allocation, with name, allocated bytes, share of parent, logical bytes and file count. Info shows scan time, volume metadata, tree totals and nonzero scan counters.

Top files, Extensions and Search share a scope directory, initially `/`. Press `u` to switch between the Tree's current directory and `/`; a non-root scope appears as `under <path>` beside the tabs. Scope changes refresh these results and reset their selections. Results stay cached during navigation, drawing, resizing and byte-format changes.

Top files uses the stored top-100 index at `/` with no filter. Otherwise it ranks up to 100 primary files within the scope. Press `f` on that tab to enter comma-separated extensions such as `mp4,mkv` or `.mp4,.mkv`, a category such as `kind:video`, or an empty value to clear the filter. The title shows the active filter. The category names are `video`, `audio`, `image`, `archive`, `disk-image`, `document`, `code`, `executable` and `other`. An unknown category displays an error and retains the previous filter.

Extensions starts with extension counts and totals in allocation order, displaying `(none)` for files without a usable suffix. Press `g` on that tab to switch between extension and category totals. Enter on a row opens Top files filtered by that extension or category in the same scope. Rankings and totals exclude duplicate hard links.

Press `/` from any tab to open Search and edit a name pattern. Enter runs a case-insensitive glob; input without `*?[{` uses substring matching, so `steam` finds `SteamLibrary`. Patterns accept backslash escapes and have a 256-byte limit. Search lists the first 1000 matches in stored-child pre-order, including files, directories, synthetic directories and duplicate hard links. It excludes the scope node itself. An empty result shows `no matches`; additional matches show `N more, refine the pattern`. Invalid patterns display an error and retain the previous search.

| Key | Action |
| --- | --- |
| `↑` / `k`, `↓` / `j` | Move selection. |
| `PgUp` / `PgDn` | Move one visible page. |
| `Home` / `End` | Select the first / last row. |
| `Enter`, `→` / `l` in Tree | Enter the selected directory. |
| `←` / `h` / `Backspace` in Tree | Return to the parent, restoring its selection and scroll. |
| `Enter` in Top files or Search | Open the node's parent in Tree with that node selected, including directory matches. |
| `Enter` in Extensions | Open Top files filtered by the selected extension or category. |
| `Tab`, `1`–`5` | Cycle tabs, or select Tree / Top files / Extensions / Info / Search. |
| `u` | Toggle the query scope between the Tree's directory and `/`. |
| `f` in Top files | Edit the file filter. |
| `g` in Extensions | Toggle extension / category grouping. |
| `/` | Open Search and edit its pattern. |
| `b` | Toggle raw byte counts across all tabs. |
| `?` | Toggle the help overlay. |
| `Esc` | Cancel an input, close help, or quit when neither is open. |
| `q`, `Ctrl-C` outside an input | Quit, including while help is open. |

The one-line editor uses `tui-input` with its crossterm backend. While it is open, characters such as `q`, digits and navigation letters are text. `←` / `→`, `Home` / `End` and `Backspace` edit the line; Enter applies it and Esc cancels. The prompt occupies the row above the footer, and horizontal scrolling keeps the input cursor visible.

While help is open, other navigation and display keys are ignored. Outside an input, press events are handled and repeat events are accepted only for movement; release events are ignored. The Extensions grouping toggle runs once per press. Resizing clamps current and saved selections and scroll positions.

## Rendering

Tree shares use each child's `total_alloc` divided by its parent's `total_alloc`, displayed as integer per-mille (`‰`) and clamped to 1000. Proportional bars use `u128` arithmetic and clamp to the bar width. A zero-allocation parent displays `—` and an empty bar. Hard-link duplicate rows carry `=` and no share bar because they do not contribute to parent totals.

Directories carry `/`, reparse points carry `↪`, and synthetic directories are dimmed. Empty directories display `(empty)`. Names escape control characters as `\u{…}` and truncate by display width with `…`; header paths, Top files paths and Search paths truncate in the middle. Only visible table rows are rendered. Below 40 columns or 8 rows, `terminal too small (minimum 40×8)` replaces the views.
