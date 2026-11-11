# Plan: request UAC elevation for drive scans

## Why

- Reading a Windows raw volume needs administrator rights. Today the user must open an elevated terminal by hand, or the scan fails with access denied.
- The planned MCP server runs unelevated under the agent host, so it needs a way to request elevation for a scan (see [mcp.md](mcp.md)).

## What Changes

- `adamite-io` can tell whether the process is elevated and can relaunch the current executable elevated through the UAC prompt.
- `adamite scan --status-file FILE` reports progress and the outcome as JSON lines.
- Every command with a drive source elevates automatically when the process is not elevated: the scan runs in an elevated child behind a UAC prompt, and the unelevated parent shows the usual progress bar and then continues the command. `--no-elevate` opts out.

## Scope

In scope: the `adamite-io` elevation and status types, automatic elevation, the CLI `--status-file` and `--no-elevate` flags, tests, the io and CLI READMEs, and ARCHITECTURE §4.1.

Out of scope:
- Cancelling an elevated scan.
- MCP scan tools; [mcp.md](mcp.md) builds them on this plan.
- The planned elevated service.

## Design decisions

1. D1. Elevation in `adamite-io`. Windows-only `ElevatedProcess` lives in the existing FFI module. It uses the `windows` crate, adding the `Win32_UI_Shell`, `Win32_System_Threading` and `Win32_Security` features.
   - `ElevatedProcess::is_current()` reads `TokenElevation` from the process token.
   - `ElevatedProcess::launch(args)`:
     - Runs `std::env::current_exe()` through `ShellExecuteExW` with verb `runas`, `SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC` and `SW_HIDE`.
     - Quotes each argument by the `CommandLineToArgvW` rules: every argument is wrapped in quotes, and backslashes before the closing quote are doubled.
     - Rejects arguments containing `"` (illegal in Windows paths) with `ElevationError::Argument`.
     - `ERROR_CANCELLED` maps to `ElevationError::Declined`.
   - `try_wait()` and `wait()` return the child's exit code (`WaitForSingleObject`, `GetExitCodeProcess`). The handle closes on drop.
   - On other platforms the type does not exist, and drive sources keep failing as today.
2. D2. Status file. `adamite-io::status` owns the line format, with `ScanStatusWriter` and `ScanStatusReader` (serde, serde_json). Each line is one JSON object:
   - `{"progress":{"phase":"stream","records":N,"planned":M|null,"bytes":B}}`
     - Written at most every 500 ms, plus on each phase change.
   - `{"done":{"saved":"<path>","bytes":N}}` as the final line on success.
   - `{"failed":{"message":"<error text>","exit_code":N}}` as the final line on failure.
   - Writes are append-only, and each line is flushed whole.
   - The reader returns only complete lines and keeps a partial trailing line for the next poll.
   - The file is created exclusively; an existing file is a usage error.
3. D3. `scan --status-file FILE`. This works on any scan source, elevated or not, and keeps the normal stdout and stderr output. `-o` is required, because the outcome names the saved snapshot.
4. D4. Automatic elevation. It applies to `scan`, `info`, `top`, `ext`, `ls` and `browse` with a drive source on Windows. Image and snapshot sources never elevate.
   - If `ElevatedProcess::is_current()` is true, or `--no-elevate` is given, the command runs in-process as today.
     - With `--no-elevate`, an access-denied drive error adds the hint `run without --no-elevate to request elevation, or use an elevated terminal`.
   - Otherwise the parent launches an elevated child before opening the drive: `scan <drive> -o <snapshot> --status-file <status> --no-elevate`, plus the parent's `--profile`, `--max-read-rate` and `--threads` values.
     - `--no-elevate` keeps the child from ever prompting again.
     - `--trace-chrome` is not forwarded.
   - The parent prints `requesting administrator access for <drive>…` to stderr before the prompt.
   - `<snapshot>`:
     - For `scan -o FILE`: FILE, with `--force` when the parent was given `--force`. The parent checks no-clobber before launching.
     - For the other commands: a new `adamite-<pid>-<n>.adm` in `std::env::temp_dir()`. The parent removes it after loading.
   - `<status>` is `.<snapshot file>.<pid>-<n>.status.jsonl` next to `<snapshot>`. The parent removes it at the end.
   - The parent:
     - Polls the status file and `try_wait()` every 100 ms and feeds the progress lines into the existing progress view.
     - On `done`, prints `scan`'s save confirmation, or loads the temporary snapshot and runs the command.
     - On `failed`, prints the child's message and exits with its code.
     - If the child exits without a final line, reports exit code 1 with the child's exit code in the message.
   - Failures to elevate:
     - A declined prompt prints `error: elevation was declined` and exits 4.
     - Any other launch failure, for example no interactive desktop, exits 4 with the system error and the `--no-elevate` / elevated-terminal hint.
   - `--no-elevate` is accepted on every platform and with every source, and has no effect when no elevation would happen.
5. D5. Tests.
   - Unit tests:
     - Argument quoting (spaces, trailing backslashes, rejected quotes).
     - Status write/read round trips, including a partial trailing line.
   - CLI tests on an image:
     - `--status-file` writes ordered progress lines and one final line, for both success and failure.
     - `--no-elevate` parsing on all sources.
   - Windows CI: the runner is already elevated, so `is_current()` is true and a drive scan runs in-process without launching a child.
   - The UAC prompt itself is checked by hand on Windows.

## Tasks

Each task ends with one reviewed commit, `deno task check` green and CI green. Each Codex run covers implementation and targeted verification and stays under 40 minutes.

- [x] E1 — `ElevatedProcess` and `adamite-io::status`, with unit tests (D1, D2).
- [x] E2 — `scan --status-file`, automatic elevation and `--no-elevate`, with CLI tests, the READMEs and ARCHITECTURE §4.1 (D3–D5).

## Acceptance criteria

- [ ] In an unelevated Windows terminal, `adamite scan C: -o C.adm` shows the UAC prompt; accepting it shows the progress bar and then the save confirmation.
- [ ] `adamite browse C:` and `adamite top C:` work the same way, and leave no temporary files behind.
- [ ] Declining the prompt prints `error: elevation was declined` and exits 4. `--no-elevate` in an unelevated terminal fails with access denied and the hint, without a prompt.
- [ ] In an elevated terminal, and for image or snapshot sources, no prompt appears and output is unchanged.
- [x] Existing CLI exact-text and JSON tests are unchanged; `deno task check` and CI pass on Linux and Windows.
