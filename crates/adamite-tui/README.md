# adamite-tui

`adamite-tui` is a read-only terminal browser for an existing `adamite_core::scan::ScanOutput`. The [CLI](../adamite-cli/README.md) loads sources, resolves paths and launches it with `adamite browse <SOURCE> [PATH]`. The library depends on core and shared formatting; source I/O stays with the caller.

## Public API

- `Browser::new(output: &ScanOutput, start: NodeId) -> Browser<'_>` borrows a completed scan and selects the starting directory. Invalid or file identifiers fall back to the root.
- `Browser::run(self) -> Result<(), TuiError>` initializes the terminal, draws views and reads input until quit, then restores the terminal. Restoration is attempted after partial initialization and after every event-loop result. A loop error takes precedence over a restoration error; ratatui's restoration panic hook is retained.
- `TuiError` is a non-exhaustive error enum. `TuiError::Io { operation, source }` identifies the failed terminal operation and preserves the underlying `std::io::Error`.

## Tabs and controls

Tree lists children by subtree allocation, with name, allocated bytes, share of parent, logical bytes and file count. Top files shows the stored top-100 file index. Extensions shows extension counts and totals in stored order, using `(none)` for empty extensions. Info shows scan time, volume metadata, tree totals and nonzero scan counters. Tabs retain independent selections and scroll positions.

| Key                     | Action                                                             |
| ----------------------- | ------------------------------------------------------------------ |
| `↑` / `k`, `↓` / `j`    | Move selection.                                                    |
| `PgUp` / `PgDn`         | Move one visible page.                                             |
| `Home` / `End`          | Select the first / last row.                                       |
| `Enter`, `→` / `l`      | In Tree, enter the selected directory.                             |
| `←` / `h` / `Backspace` | In Tree, return to the parent, restoring its selection and scroll. |
| `Enter` in Top files    | Open the file's parent in Tree with that file selected.            |
| `Tab`, `1`–`4`          | Cycle tabs, or select Tree / Top files / Extensions / Info.        |
| `b`                     | Toggle raw byte counts across all tabs.                            |
| `?`                     | Toggle the help overlay.                                           |
| `Esc`                   | Close help if open; otherwise quit.                                |
| `q`, `Ctrl-C`           | Quit, including while help is open.                                |

While help is open, other navigation and display keys are ignored. Press events are handled; repeat events are accepted only for movement (`↑`/`↓`, `k`/`j`, page keys and endpoints); release events are ignored. Resizing clamps current and saved selections and scroll positions.

## Rendering

Tree shares use each child's `total_alloc` divided by its parent's `total_alloc`, displayed as integer per-mille (`‰`) and clamped to 1000. Proportional bars use `u128` arithmetic and clamp to the bar width. A zero-allocation parent displays `—` and an empty bar. Hard-link duplicate rows carry `=` and no share bar because they do not contribute to parent totals.

Directories carry `/`, reparse points carry `↪`, and synthetic directories are dimmed. Empty directories display `(empty)`. Names escape control characters as `\u{…}` and truncate by display width with `…`; header paths and Top files paths truncate in the middle. Only visible table rows are rendered. Below 40 columns or 8 rows, `terminal too small (minimum 40×8)` replaces the views.
