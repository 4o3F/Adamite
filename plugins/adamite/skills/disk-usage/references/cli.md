# CLI fallback

Use these commands when Adamite MCP tools are absent. Install the `adamite` binary separately on PATH. Query an existing `.adm` file to avoid starting a scan while investigating usage. Replace `C.adm` with the selected snapshot's path and quote paths containing spaces.

The command forms below were checked against `adamite <command> --help` from `cargo build -p adamite-cli --locked` (debug build, version 0.1.0-beta.1).

## Query commands

```sh
adamite info C.adm --json
adamite tree C.adm / --budget 60 --min-share 0.5 --json
adamite tree C.adm /Users --budget 100 --min-share 1 --json
adamite ls C.adm /Users --offset 0 --limit 50 --json
adamite top C.adm --n 20 --json
adamite top C.adm --under /Users --ext mp4,mkv --ext .webm --n 20 --json
adamite top C.adm --under /Users --kind video --n 20 --json
adamite ext C.adm --under /Users --by extension --n 20 --json
adamite ext C.adm --under /Users --by kind --n 20 --json
adamite search C.adm '*.iso' --under /Users --offset 0 --limit 50 --json
```

The examples use POSIX shell quoting; use `"*.iso"` in Windows Command Prompt. PATH is relative to the volume root, defaults to `/` for `tree` and `ls`, and accepts `/` or `\` separators. Exact names win over Unicode lowercase matches. Use a decimal direct-child selector such as `/Users/#1234` when a name is ambiguous. `--under` defaults to `/`.

| Command | Defaults and bounds |
| --- | --- |
| `tree` | `--budget 60`, allowed 1–400, counts root and collapsed rows. `--min-share 0.5` is a finite percentage 0–100, rounded down to basis points. |
| `ls` | `--offset 0`, `--limit 50`. These are nonnegative machine-sized integers; offset + limit must fit. Zero limit is allowed, and there is no MCP-style 500-row cap. |
| `top` | `--n 20`, allowed 1–100. `--ext` accepts comma-separated suffixes and may repeat; it conflicts with `--kind`. One leading dot is removed and ASCII case is ignored. `--ext ''` matches files without a usable extension. |
| `ext` | `--n 20`, any positive u64; `--by extension` or `kind`. The CLI has no MCP-style 200-group cap. |
| `search` | Required pattern, at most 256 bytes; `--offset 0`, `--limit 50`, allowed 1–500. |

Kinds are `video`, `audio`, `image`, `archive`, `disk-image`, `document`, `code`, `executable`, `other`. Search matches case-insensitive whole-name globs (`*`, `?`, `[abc]`, `{a,b}`) with backslash escaping; without `*?[{` it matches a substring. Quote patterns to prevent shell expansion.

`search.next_offset` supplies the next page. For `ls`, advance the offset by the number of returned items until `total_children` is reached. Narrow a large tree by PATH; narrow rankings and totals with `--under`. CLI output has no MCP 32 KiB response cap, so keep budgets and limits small.

## Inspecting candidates and age

`info` reports the saved scan time in `snapshot.scanned_at`. Compare that time to the current time and report the snapshot's age; absent or future times are unknown. CLI JSON does not add MCP's `age_seconds` or `stale` fields. Use 24 hours as the default stale threshold when no other threshold was selected.

There is no CLI `snapshots` or `node_info` subcommand. Ask for a saved snapshot path if none is known. Inspect a candidate through `ls` on its parent or a narrowly scoped `search`; these return flags and self/subtree sizes. `top` also returns `mtime_filetime` (a decimal FILETIME string, 100-nanosecond ticks since 1601-01-01). Hard-link counts and node-detail timestamps require MCP `node_info`; state which candidate metadata is unavailable before recommending anything. Keep allocation, logical size, modification age and scan age distinct.

## Missing or corrupt snapshots

Ask before proposing a scan. For an unelevated Windows drive scan, explain that Windows UAC will ask for administrator access. Elevation by the agent remains restricted to MCP `scan_volume` after consent. If MCP is absent, give the user a command to run themselves, with an absolute destination in a writable snapshot directory:

```text
adamite scan C: -o "C:\Adamite\snapshots\C.adm" --json
adamite scan C: -o "C:\Adamite\snapshots\C.adm" --force --json
adamite scan volume.img -o "/absolute/snapshots/volume.adm" --force --json
```

Use `--force` only when replacing the intended snapshot. `scan` takes an NTFS volume image or a Windows drive written as `C:`; a saved `.adm` is a query source. `-o` is also spelled `--output`. Scan resource flags are `--profile background|balanced|max` (balanced by default), `--max-read-rate BYTES` and `--threads N`; saved-snapshot queries ignore them. With `-o`, JSON reports `saved` and `bytes`. Wait for the user's scan to finish before querying its output.

Never delete, move or run cleanup commands, and never retry declined elevation without a new agreement. CLI failures print `error:` and source details on standard error and leave standard output empty; inspect that message before retrying. Exit codes are 2 for usage, 3 for snapshot codec failures, 4 for permission/elevation failures and 1 for other failures.
