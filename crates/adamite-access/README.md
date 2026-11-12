# adamite-access

`adamite-access` loads NTFS images, Windows raw volumes and saved `.adm` snapshots. Background jobs report progress and publish drive scans; the catalog discovers saved snapshots. The crate also runs elevated scan children, constructs the `adamite/1` reports, and writes text or JSON to a caller-supplied writer. It uses [adamite-core](../adamite-core/README.md) for snapshots and queries, [adamite-io](../adamite-io/README.md) for file access and scanning, and [adamite-format](../adamite-format/README.md) for display values.

## Public API

| Path | Behavior |
| --- | --- |
| `Source` | `open(path)` detects drives, snapshots and images. `open_snapshot(path)` accepts only a file selected by an `.adm` extension ignoring ASCII case or the complete ADM magic; images and drives return `SourceError::UnsupportedSource`. `is_snapshot()` and `path()` expose the detected source. |
| `Source::load_observed(self, &ScanLimits, observer)` | Decodes a snapshot or scans an image or Windows drive. The `adamite_io::limits::ScanLimits` value controls scans, and the callback receives `adamite_io::driver::DriverProgress`. Snapshot loads do not call the observer. |
| `PublishMode`, `SnapshotFile` | `new(path, mode)` selects no-clobber or replacement publication. `preflight(source)` rejects conflicting destinations; `save(output)` writes, synchronizes and publishes a same-directory temporary file; `length()` reads the saved byte length. |
| `StatusFile` | `create(path)` exclusively creates the JSON lines status file. `progress(report)` retains the first write error; `check()` takes it; `finish(status)` writes and flushes the caller's final `ScanStatus`. |
| `ScanOptions`, `ReadRate` | Stores explicit profile, rate, worker and elevation choices. `ReadRate::new(argument, bytes)` pairs validated positive bytes with the original text. `limits()` resolves `ScanLimits`; `child_arguments()` forwards only explicit profile, rate and worker choices in CLI order. |
| `ElevatedScan` | `required(options)` checks Windows elevation policy. `new(destination)` owns unique scan paths; `launch(drive, options)` starts the current executable with `scan`; `run(drive, options, observer_factory)` monitors, waits and loads the completed snapshot. |
| `ElevatedJob`, `JobState`, `CompletedScan` | `poll()` reads status and checks exit without sleeping, returning the latest progress or the saved outcome. Dropping the job closes handles before removing its status file and temporary snapshot; explicit snapshot destinations are preserved. |
| `ScanJob`, `JobStatus` | `open(path, options, catalog)` loads a file or reuses a listed Windows drive's saved snapshot; `scan_drive(letter, options, directory)` scans and replaces `<LETTER>.adm`. `poll()` returns the latest `DriverProgress`, `Done(Arc<ScanOutput>, Option<PathBuf>)`, or `Failed(JobError)`. `path()` and `is_loading()` describe the current operation for callers that display it. |
| `JobError` | Retains the complete human message, including causes, and an optional child exit code as `i64`, preserving signed status codes and Windows process codes. `from_error(error, exit_code)` formats the same cause chain for job and CLI messages. |
| `SnapshotCatalog` | `new(SnapshotDir)` selects a directory. `entries()` refreshes its sorted metadata listing; `for_drive(letter)` finds the first matching single-letter stem ignoring ASCII case. `claim_drive(entries, letter)` removes that match from an existing listing. `directory()` exposes the publication directory. Missing directories are empty, unreadable entries retain their errors, and directory-open failures are returned. |
| `ScanJobError` | Reports publication, status, elevation and child failures, preserving child messages and exit codes; completed-snapshot loading errors retain `SourceError`. |
| `SourceError` | Preserves the operation, path and cause of source I/O, snapshot decoding and scan failures; distinguishes unsupported sources and raw drives requested outside Windows. |
| `ReportError` | Reports missing snapshot data, page range errors, path lookup failures, query and tree errors, JSON serialization errors and output I/O failures. |
| `DirectoryLookup` | Adds `resolve_directory(path)` to `Snapshot`, reporting a file's canonical path when it is not a directory. |
| `report::{InfoReport, SavedReport, TopReport, ExtReport, KindReport, SearchReport, LsReport}` | Constructs volume information, save confirmations, file rankings, grouped statistics, search pages and directory pages. The corresponding row types are `TopItem`, `ExtItem`, `KindItem`, `SearchItem` and `LsItem`. |
| `report::{Envelope, SnapshotReport, Units, Volume, Tree, Stats}` | Shared schema, source identity, scan time, units, volume geometry, tree totals and scan counters. `Stats::entries()` returns the counters in core field order. |
| `render::Report` | Provides `write_text(writer, raw_bytes)` alongside Serde serialization. |
| `render::Renderer` | `new(json, raw_bytes)` selects output format. `write(report, writer)` writes text or one JSON line with raw byte counts. |
| `render::json::Json` | Writes any borrowed serializable value as one JSON line. |

Source detection opens snapshot files but defers decoding until loading. An `.adm` extension selects even a damaged or truncated snapshot; decoding failures are returned without scanning. Raw drives use an ASCII drive letter followed by `:`, such as `C:`. Images start at NTFS volume byte zero.

The [CLI](../adamite-cli/README.md) owns command-line parsing, exit codes, error printing and hints, standard output, terminal progress and the adapter from access jobs and catalog entries to the TUI. It converts parsed command-line limits to `ScanOptions` and supplies final failure messages and exit codes to `StatusFile`. It passes its progress observer into this crate and maps library errors to its existing error variants.

`ScanJob` is `Send`: callers can move it to another thread for polling. In-process scans and file loads run on detached workers. UAC launch and child polling share a worker thread; after successful publication, a separate worker loads the saved snapshot. Each poll drains pending progress and returns only the latest report; absent progress leaves previously displayed counters to the caller. A successful output is transferred once, so callers stop polling after completion. Drive jobs return the published path; file loads and unsaved image scans return no publication path. Dropping a job detaches in-process work and lets an elevated child continue publishing.

## Verification

Run `cargo test -p adamite-access --locked` for library coverage. Run `deno task check` from the repository root for formatting, Clippy on the host and Windows targets, workspace tests, documentation and dependency checks.
