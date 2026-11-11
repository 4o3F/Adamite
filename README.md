# Adamite

Adamite scans NTFS images and Windows raw volumes into an immutable snapshot of files, directories and disk usage. The `adamite` command saves scans as `.adm` files, reports sizes and scan diagnostics as text or JSON, and opens an interactive terminal browser. Saved snapshots can be queried on Windows or Linux without elevated device access.

## Downloads

Tagged versions are distributed through [GitHub Releases](https://github.com/4o3F/Adamite/releases). Development builds are available from completed [CI runs in Actions](https://github.com/4o3F/Adamite/actions/workflows/ci.yml), under the artifacts `adamite-Linux` and `adamite-Windows`.

Each platform artifact contains a package and its `.sha256` checksum: `adamite-<version>-x86_64-unknown-linux-gnu.tar.gz` for Linux x64, or `adamite-<version>-x86_64-pc-windows-msvc.exe` for Windows x64. Extract the Linux archive, or rename the Windows executable to `adamite.exe`, and place the binary on your `PATH`.

## Quick start

In an elevated Windows terminal, scan the NTFS C: volume and save the result:

```text
adamite scan C: -o C.adm
```

Then use an ordinary terminal to browse or query the saved file:

```text
adamite browse C.adm
adamite info C.adm
adamite top C.adm --n 20
adamite ext C.adm --json
adamite ls C.adm /Users --limit 50
```

The snapshot must be readable from the ordinary terminal. On either platform, an uncompressed NTFS volume image can replace `C:` as the scan source. See the [CLI README](crates/adamite-cli/README.md) for source detection, flags, paths and the JSON schema, and the [TUI README](crates/adamite-tui/README.md) for keyboard controls.

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
| [adamite-tui](crates/adamite-tui/README.md)           | Read-only terminal browsing of a completed scan.                                          |
| [adamite-format](crates/adamite-format/README.md)     | Shared display formatting for the CLI and TUI.                                            |
| [adamite-io](crates/adamite-io/README.md)             | Slice, image and Windows raw-volume readers with a synchronous scan driver.               |
| [adamite-core](crates/adamite-core/README.md)         | Scanning, tree aggregation, immutable snapshots and `.adm` persistence.                   |
| [adamite-ntfs](crates/adamite-ntfs/README.md)         | `no_std` NTFS byte decoding.                                                              |
| [adamite-fixtures](crates/adamite-fixtures/README.md) | Shared development fixtures, independent reference reconstruction and synthetic builders. |
