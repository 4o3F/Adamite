# Adamite

Adamite scans NTFS images and Windows raw volumes into an immutable snapshot of files, directories and disk usage. The `adamite` command saves scans as `.adm` files, reports sizes and scan diagnostics as text or JSON, and opens an interactive terminal browser. Saved snapshots can be queried on Windows or Linux without elevated device access.

## Downloads

Tagged versions are distributed through [GitHub Releases](https://github.com/4o3F/Adamite/releases). Development builds are available from completed [CI runs in Actions](https://github.com/4o3F/Adamite/actions/workflows/ci.yml), under the artifacts `adamite-Linux` and `adamite-Windows`.

Each platform artifact contains a package and its `.sha256` checksum: `adamite-<version>-x86_64-unknown-linux-gnu.tar.gz` for Linux x64, or `adamite-<version>-x86_64-pc-windows-msvc.exe` for Windows x64. Extract the Linux archive, or rename the Windows executable to `adamite.exe`, and place the binary on your `PATH`.

## Quick start

Run `adamite` or `adamite browse` in an interactive terminal to open Home. Select a Windows NTFS drive or a saved snapshot, or use `Open file…` for an image or snapshot path. Drive scans request administrator access when needed and save their result for the next launch. Press `r` to rescan a drive, `o` to return from the browser to Home, or `q` to quit. See [snapshot locations](crates/adamite-cli/README.md#home-and-saved-scans).

In an elevated Windows terminal, scan the NTFS C: volume and save the result:

```text
adamite scan C: -o C.adm
```

Then use an ordinary terminal to browse or query the saved file:

```text
adamite browse C.adm
adamite info C.adm
adamite tree C.adm / --budget 60 --min-share 0.5
adamite top C.adm --n 20
adamite ext C.adm --json
adamite ls C.adm /Users --limit 50
```

The snapshot must be readable from the ordinary terminal. On either platform, an uncompressed NTFS volume image can replace `C:` as the scan source. See the [CLI README](crates/adamite-cli/README.md) for source detection, flags, paths and the JSON schema, and the [TUI README](crates/adamite-tui/README.md) for keyboard controls.

## MCP server and Claude Code plugin

Run `adamite mcp` to serve snapshot discovery, disk-usage queries and consent-based scan jobs over MCP stdin/stdout. It discovers saved scans in the [snapshot directory](crates/adamite-cli/README.md#home-and-saved-scans); use `adamite mcp --snapshot C.adm` to include a specific file. Queries include budgeted trees, directory pages, largest files, extension/category totals, name search and node details. See the [MCP README](crates/adamite-mcp/README.md) for all ten tools, parameters, limits and errors.

For Claude Code, run `/plugin marketplace add 4o3F/Adamite`, then `/plugin install adamite@adamite`. The plugin registers `adamite mcp` and installs the `disk-usage` Skill, which asks before scanning and presents candidates for the user to review. The `adamite` binary must already be on PATH. See the [plugin README](plugins/adamite/README.md) for local installation, manual MCP/Skill setup and permission rules.

## Build and develop

Install Rust through rustup and use the toolchain selected by [rust-toolchain.toml](rust-toolchain.toml) (currently Rust 1.99). From the repository root:

```text
cargo build --release -p adamite-cli --locked
```

The binary is `target/release/adamite` on Linux or `target/release/adamite.exe` on Windows. With Deno 2.9.8 and cargo-deny installed, run the development gate:

```text
deno task check
```

The gate checks Rust formatting, Clippy (including Windows cross-target checks), tests, documentation, dependency policy, the parser's `no_std` build, Deno scripts and workflow YAML. Fixture generation and bounded fuzz runs have separate tasks; see [ARCHITECTURE](docs/ARCHITECTURE.md).

## Workspace

| Crate                                                 | Responsibility                                                                            |
| ----------------------------------------------------- | ----------------------------------------------------------------------------------------- |
| [adamite-cli](crates/adamite-cli/README.md)           | The `adamite` binary: source loading, saving, reports and launching the browser.          |
| [adamite-access](crates/adamite-access/README.md)     | Shared source loading, reports, snapshot discovery, publication and scan jobs.            |
| [adamite-mcp](crates/adamite-mcp/README.md)           | MCP stdio server with snapshot queries, caching and scan control.                         |
| [adamite-tui](crates/adamite-tui/README.md)           | Source selection, scan progress and read-only snapshot browsing.                          |
| [adamite-format](crates/adamite-format/README.md)     | Shared display formatting for the CLI and TUI.                                            |
| [adamite-io](crates/adamite-io/README.md)             | Slice, image and Windows raw-volume readers with a synchronous scan driver.               |
| [adamite-core](crates/adamite-core/README.md)         | Scanning, tree aggregation, immutable snapshots and `.adm` persistence.                   |
| [adamite-ntfs](crates/adamite-ntfs/README.md)         | `no_std` NTFS byte decoding.                                                              |
| [adamite-profile](crates/adamite-profile/README.md)   | Requested heap counters and an opt-in Chrome trace subscriber for executables.            |
| [adamite-fixtures](crates/adamite-fixtures/README.md) | Shared development fixtures, independent reference reconstruction and synthetic builders. |
