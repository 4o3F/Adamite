# Plan: show scan progress in the CLI

## Why

- A scan of a large volume runs for seconds to minutes (longer under the read-rate profiles) and the CLI prints only `scanning <source>…`, so users cannot tell whether it is working or how long remains.
- Core already reports `ScanProgress` (phase, records processed) on every step, but it carries no total, and `SyncDriver::run` discards it.

## What Changes

- `ScanProgress` reports the planned MFT record count once it is known, so callers can show a percentage.
- `SyncDriver` passes every progress report, plus bytes read, to an optional caller-supplied observer.
- The CLI shows a progress bar on standard error while scanning when standard error is a terminal; otherwise it keeps today's single `scanning <source>…` line.

## Scope

In scope: core `ScanProgress`, the `SyncDriver` observer, the CLI progress display for every command that scans (including `browse` before the TUI starts), tests and READMEs.

Out of scope: progress for `.adm` save/load (sub-second at 1M records), a TUI-embedded progress view, `--json` progress events, changes to scan behaviour or output.

## Design decisions

1. D1. Core. `ScanProgress` gains `records_planned: Option<u64>`: `None` until the MFT data size is known (boot, record 0, discovery), then the planned record count from the MFT data size and record size. `records_processed` keeps its meaning. Core still performs no I/O and reads no clocks; the extra field is a copy of existing state.
2. D2. Driver. `SyncDriver::run` stays and delegates to one new method, `run_observed(reader, observer)`, where the observer receives a `DriverProgress { scan: ScanProgress, bytes_read: u64 }` after each scanner progress step and after each served batch. There is one driving loop; `run` passes a no-op observer. Observers cannot fail or cancel (cancellation stays future service work).
3. D3. CLI display. Use the `indicatif` crate on standard error, created only when standard error is a terminal (no hand-written terminal handling).
   - During `Stream`: a bar of records processed over records planned, with percentage, bytes read, read rate and ETA.
   - Before the total is known and during merge/link/build/aggregate/finalize: a spinner whose message names the phase (for example `linking`).
   - The bar is cleared when the scan ends or fails, then the existing output follows unchanged. Non-terminal standard error keeps the current `scanning <source>…` line, so existing CLI exact-text tests and piped/JSON usage are unchanged.
   - Redraws are throttled by indicatif (its default draw rate), so the observer stays cheap.
4. D4. Tests.
   - Core: `records_planned` is `None` before discovery and equals the slot count during and after `Stream` on fixtures and synthetic volumes.
   - Driver: observed reports are monotonic in records and bytes, end at the planned count, and `run` and `run_observed` produce identical output.
   - CLI: non-terminal stderr output is unchanged (existing tests); the progress view's phase labels and template are covered by unit tests.

## Tasks

Each task ends with one reviewed commit, `deno task check` green and CI green; each Codex run stays under 40 minutes.

### P1 — Progress reporting and CLI bar

- [x] Implement D1–D4.

## Acceptance criteria

- [ ] Scanning a drive or image in a terminal shows a moving bar with percentage and ETA during record streaming and a phase label afterwards; the bar disappears before results print.
- [x] Piped or redirected stderr, `--json` output and every existing CLI exact-text test are unchanged.
- [ ] `deno task check` and CI pass; core and CLI READMEs describe the progress reporting.

