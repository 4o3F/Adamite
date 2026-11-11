# Adamite Architecture

Adamite gives users and agents a size-aggregated view of NTFS filesystems. The libraries read NTFS images and Windows raw volumes, parse Master File Table (MFT) bytes and produce an immutable `Snapshot` with `ScanStats`. The `adamite` CLI saves and queries snapshots and launches a terminal browser; MCP and service front ends remain planned.

The project is split into two layers:

- **Core layer:** scanning, tree building and snapshot persistence, with query operations planned.
- **Access layer:** CLI reports and terminal browsing, with MCP and snapshot/scan-job services planned.

Implementation language: Rust (Cargo workspace).

## 1. Overall Architecture

The diagram shows implemented dependencies. Planned MCP, HTTP/gRPC and service front ends will add snapshot caching, scan jobs, staleness and privilege management above the libraries.

```text
adamite-cli (adamite binary)
  ├── adamite-tui: borrowed ScanOutput → interactive browser
  │     ├── adamite-format: shared terminal display formatting
  │     │     └── adamite-core
  │     └── adamite-core
  ├── adamite-format
  ├── adamite-io: VolumeReader + SyncDriver
  │     ├── SliceReader / ImageReader / RawVolumeReader (Windows)
  │     └── adamite-core: step() / feed(id, bytes)
  └── adamite-core: scan results, immutable reads, .adm codec
        ├── scan/: request protocol + bounded phase driver
        ├── mft/: bootstrap → discover → ingest → merge
        ├── graph/: entries + parent/hard-link resolution
        ├── snapshot/build/: arena/CSR → aggregate → finalize
        ├── snapshot/: nodes, names, indexes + volume info
        ├── snapshot/persist/: canonical .adm v1 encoding/decoding
        └── adamite-ntfs: boot/record/attribute/value/utf16 decoding

adamite-fixtures: development-only images, manifests and builders
```

### Workspace layout

```text
crates/
  adamite-ntfs/     # implemented: boot/record/attribute/value/utf16, no_std
  adamite-core/     # implemented: scan/mft/graph/snapshot/error/util
  adamite-fixtures/ # implemented: shared development-only fixture support
  adamite-io/       # implemented: VolumeReader + slice/image/Windows + SyncDriver
  adamite-format/   # implemented: shared sizes, escaping, time and info fields
  adamite-tui/      # implemented: read-only terminal browser
  adamite-cli/      # implemented: adamite binary, reports and snapshot file I/O
  adamite-profile/  # implemented: requested heap counters, span recorder and Chrome traces
  adamite-bench/    # implemented: synthetic scan/persistence resource measurements
  adamite-service/  # planned: snapshots, jobs, privilege and resource scheduling
  adamite-mcp/      # planned
```

| Implemented crate  | Responsibility                                                                                                                                                               |
| ------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `adamite-ntfs`     | `no_std` byte decoding through `boot`, `record`, `attribute`, `value` and `utf16` modules; record parsing owns fixup, and attribute runlists live under `attribute::runlist` |
| `adamite-core`     | Single-threaded `std` scan driver, private NTFS reading/merge and graph resolution, snapshot construction, immutable reads and `.adm` persistence                            |
| `adamite-io`       | Exact positional `VolumeReader`, `SliceReader`, `ImageReader`, Windows `RawVolumeReader` and synchronous `SyncDriver`; adapter-owned alignment and volume context            |
| `adamite-format`   | Shared byte sizes, escaped terminal text, extension labels, scan timestamps, volume serials and ordered info fields                                                          |
| `adamite-tui`      | `Browser` and `TuiError`; read-only Tree, Top files, Extensions and Info tabs over a borrowed `ScanOutput`                                                                   |
| `adamite-cli`      | The `adamite` binary: source classification, scan/save, text/JSON reports, path resolution and browser startup                                                               |
| `adamite-profile`  | Unpublished executable support: counting System allocator, span heap events, in-memory span summaries and an opt-in Chrome trace guard                                       |
| `adamite-fixtures` | Unpublished development support: image loading, independent manifests/reference MFT reconstruction, synthetic builders and streaming volumes                                 |
| `adamite-bench`    | Unpublished binary: cached sparse synthetic volumes, isolated production scan/encode/decode measurements and optional Chrome traces                                          |

Parser operations belong to their types: `boot::VolumeGeometry::from_boot_sector` decodes volume geometry, `record::FileRecord::parse` validates headers and restores protected trailers in place, and `record::RecordClass::classify` applies streaming classification precedence. Parsing errors leave record bytes unchanged. Typed attribute decoders live under `value`; record references and system record numbers live under `record::reference` and `record::system`. Every public parser item has one module path.

Core performs no file, device or network I/O. It starts no threads, reads no clocks and never sleeps. Persistence receives sequential bytes through a caller-supplied fallible copy callback; file handles, buffering and seeking stay in `adamite-io`. `adamite_io::reader::VolumeReader` defines the exact positional read contract, and `adamite_io::driver::SyncDriver` owns synchronous transport and scheduling. `adamite_core::scan::ScanContext` supplies optional label, scan FILETIME and free bytes. Before constructing the driver, image callers obtain the current scan timestamp through `ImageReader::context()`, with label and free bytes absent; Windows raw-volume callers obtain all three through `RawVolumeReader::context()`.

The public core modules are `scan`, `snapshot` and `error`. `adamite_core::scan::Scanner` drives phase changes and owns request identifiers, queue capacity, waiting decisions and the shared per-call work budget. Configuration, protocol, status and `ScanStats` definitions live in private scan children and are exported directly under `scan::`. `scan::ScanOutput::{to_bytes, from_bytes}` encode and decode complete scan outputs, with failures exposed as `snapshot::PersistError`. `adamite_core::snapshot::Snapshot` exposes immutable reads, and `adamite_core::error::ScanError` carries fatal stage failures. The crate root contains module declarations without item re-exports.

Private `mft/` owns bootstrap, discovery, mapping, ingestion, decoding and merge. Its stages return byte ranges, planning commits and consumption progress; only the scan driver handles request slots and ids. MFT merge constructs `graph::entry::EntryRecord` values, and `graph::link::Linker` resolves their names and parents into snapshot-owned build inputs. Private `snapshot/build/` contains separate bounded builder, aggregation and finalization passes; storage and its mutable access stay within snapshot modules. `util.rs` supplies domain-free fallible collection/counter traits, while `error.rs` wraps the snapshot error leaf. Configuration and statistics remain independent leaves.

Graph depends on snapshot types/build inputs; MFT depends on graph, and the scan driver orchestrates MFT, graph and snapshot build passes. Private `snapshot/persist/` encodes snapshot storage and scan counters, with the dependency edge `snapshot::persist → scan::{config, stats}` for `ScanContext` and `ScanStats`; it does not depend on scan status, protocol or scanner. NTFS contribution-to-entry conversion stays in MFT, so graph does not depend on NTFS contribution models. Pipeline operations belong to their record, mapping, contribution, linker or pass types. Snapshot persistence is described in §3; implemented CLI and synchronous I/O, plus planned parallelism, query, service, MCP and USN components, are described in §4–§6.

`adamite-cli` owns source detection, image/drive scanning through `adamite-io`, snapshot file I/O, path resolution and exit codes. `adamite-tui` borrows a completed `ScanOutput` through `Browser`, owns terminal setup/restoration and never loads sources. Both use `adamite-format` for display values; core owns scan-counter names, node classification and flag names.

### Key decisions

| Decision                                                                                                       | Rationale                                                                                                                                                                 |
| -------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `adamite-ntfs` and `adamite-core` do no I/O                                                                    | Caller-owned reads allow the same parsing/scan code to use memory images, platform adapters and fuzz inputs.                                                              |
| The I/O adapter owns native alignment and volume context                                                       | Raw reads expand to complete logical sectors in physically aligned storage; the adapter queries labels/free bytes and reads the clock before callers supply core context. |
| Unsafe code is confined to `adamite-io/src/reader/windows/ffi.rs` and `adamite-profile/src/allocator.rs`       | Safe, typed wrappers isolate Windows device-control and volume-query calls; the counting allocator forwards the `System` allocation contract.                             |
| MFT mapping comes from record 0's own runlist and attribute-list discovery, not `FSCTL_GET_RETRIEVAL_POINTERS` | Live volumes and images can use the same bootstrap algorithm.                                                                                                             |
| Snapshot is immutable and can be shared through `Arc`                                                          | Multiple consumers can read it without locking; cloning Snapshot itself copies its arrays.                                                                                |
| Scanning is decoupled from queries; `.adm` persistence is implemented in core                                  | The CLI saves elevated scan results for unelevated queries (§4.1).                                                                                                        |
| A planned walkdir fallback emits the same EntryRecord boundary                                                 | FAT/exFAT/ReFS/network drives could reuse tree building without an NTFS parser.                                                                                           |

## 2. Scan Pipeline

### 2.1 Phases and driver protocol

| Pipeline phase | Scanner states | Work |
|---|---|---|
| 0 | `Boot → Record0 → Discover` | Validate geometry, read record 0 and discover the complete MFT mapping |
| 1 | `Stream` | Assemble and classify records in virtual order; retain base contributions and extension patches |
| 2 | `Merge` | Validate patches, merge stream segments and emit `EntryRecord` values |
| 3 | `Link` | Select names, resolve parents, repair cycles and append names to the shared pool |
| 4 | `Snapshot` | Build the node arena, cold columns and CSR child index |
| 5 | `Aggregate` | Build BFS order and accumulate checked subtree totals in reverse |
| 6 | `Finalize` | Sort children and build top files and extension statistics |

`Scanner::step()` returns `Need`, `Progress` or one `Done`; fatal errors enter `Failed`. Calls after delivery or failure return an inactive error. A phase-work tracing span is entered and dropped within each call. CPU passes process at most 256 items before yielding, with at most one directory sort per call; bulk allocation/resize/fill and loops over the small set of attribute lists may run within a call. This is a work-count bound, not a wall-clock deadline.

`ScanProgress` carries the phase, processed records, outstanding requests and input-wait status. Its `records_planned: Option<u64>` is absent during bootstrap and discovery, then holds the MFT DATA size divided by record size from the first `Stream` report through completion, including unused slots. This count is derived from MFT state without I/O or clocks.

`ScanConfig` defaults to one outstanding request and 1 MiB chunks. Chunks must be multiples of 512 between 512 bytes and 16 MiB; cap must be positive and cap × chunk must fit `usize`. Every request has a checked, monotonic `RequestId`, a volume-relative byte offset and a nonzero length no larger than the chunk bound.

`feed(id, bytes)` copies an exact response and retains no borrow. Responses may arrive in any order; slots remain occupied until consumed in virtual order. Unknown/consumed ids, duplicates, wrong lengths and inactive scans are rejected without changing state. `Progress.waiting_for_input` asks the caller to wait for its own reads. Feed pending responses before polling again when waiting; other progress calls need no input.

### 2.2 Bootstrap and streaming

1. Request offset 0, length 512, and decode volume geometry through `adamite-ntfs`.
2. Request record 0 at `mft_lcn × cluster_size`, splitting its assembly at the chunk bound. Require an in-use base with the correct identity and an unnamed non-resident DATA first segment. Its data size must be positive and record-aligned.
3. Validate every run and iterator terminator. Acquire resident attribute lists from record bytes and non-resident lists from their own mappings, up to the parser's 256 KiB value bound.
4. Track references by segment FileRef, attribute type, raw name, instance id and lowest VCN. Read an extension only when its whole virtual record range is mapped. Validate its sequence, base FileRef and segment identity before adding runs; inspect it for further list descriptors.
5. Deduplicate discoveries, reject conflicting/overlapping mappings and require gap-free coverage of the logical MFT range. An exhausted pass with inaccessible targets or lists returns `BootstrapIncomplete`; stale references, missing attributes and malformed values are fatal. Sparse, compressed or encrypted bootstrap storage is unsupported. There is no MFT-mirror fallback.
6. Stream every logical MFT ordinal. Requests stop at extent and chunk boundaries; core assembles records across responses and extents before classification. The deleted record 4095 in `frag-512` crosses the first extent boundary and is still assembled and counted. `$BITMAP` skipping is deferred.

The caller never needs to supply the whole raw MFT at once. Streaming transport storage is bounded by cap × chunk plus one record; bootstrap additionally assembles attribute-list values. Owned record/name/stream metadata grows with the scan, so the transport bound is not a total memory limit.

### 2.3 Record ingestion and linking

The parser classifies unused, BAAD, deleted, base, extension and corrupt records. In-use records receive fixup once, using 512-byte protection strides even on 4Kn volumes. Each live NTFS 3.1 stored record number must match its virtual ordinal. Deleted headers may leave it zero; nonzero mismatches are counted as corrupt. Bootstrap rereads never publish contributions or increment streaming record counts.

Attributes and required runlists are exhausted before publishing a record. While validating runs, decode retains each non-resident segment's mapped-cluster count, counting runs with an LCN, including LCN zero, and excluding holes. Ordinary parse failures discard its entire contribution and increment diagnostic counters; deleted/BAAD/unused records produce no nodes. Invalid geometry/bootstrap, a missing or corrupt directory root, arithmetic/representation overflow and allocation failure terminate the scan. Reparse tags are retained; targets are never followed.

Linking consumes only `EntryRecord`. It deduplicates identical links and drops DOS namespace 2 names when the same parent FileRef has a non-DOS name; DOS-only names remain. Distinct non-DOS names, including same-parent hard links, become separate nodes. Primary selection uses `(parent record, raw UTF-16 name, namespace, parent sequence)` ascending; secondary nodes have `HARDLINK_DUP`.

Record 5 is required and becomes NodeId 0 named `/`; its self-parent name creates no edge. Parents must be surviving directory entries with matching sequences. Children of a multi-name directory attach to its primary node. Unresolved links and every cycle-member directory are reparented under `<orphaned>`; descendants stay beneath their directories. Cycle detection is iterative.

In-use bases 0–15 except root and direct children of `$Extend` are grouped under `<NTFS metadata>`; descendants remain beneath them. Synthetic directories are created only when needed and have zero self sizes and no FileRef. Nameless records receive `<record N>` names. A live-volume scan is not atomic; coherent source bytes are the caller's responsibility, and no USN checkpoint is acquired.

## 3. Data Structures

### 3.1 Scan-time ownership

`Vec<RecordSlot>` grows in ordinal order and holds classification, optional sequence/record flags and an optional owned base contribution. Contributions retain raw UTF-16 names, SI metadata, reparse tags and stream segments. Extension `Patch` values carry base and source FileRefs plus their owned contribution, including attribute instance ids; extensions may precede bases because merge follows the full stream pass.

A stream key is `(attribute type, raw UTF-16 name)`. Segment identity adds lowest VCN, source FileRef and attribute id. Only `lowest_vcn == 0` supplies header sizes. Ordinary non-resident allocation sums mapped-cluster counts across all segments and multiplies by cluster size, including preallocation; sparse/compressed flagged streams use first-segment `compressed_size`, and resident streams use zero. Duplicate resident streams, conflicting first segments, gaps and overlaps discard the affected ordinary base. Missing/stale-base patches are rejected without modifying the base. Merge produces the scalar sizes, flags, metadata and names in `EntryRecord`.

Linking retains raw names for alias selection and ordering, then writes lossy UTF-8 directly into one shared pool. Its intermediate `ParentTree` is a flat parent-index array; each `ResolvedNode` holds only a name offset/length. Raw names are released after their record's nodes are emitted. There is no per-node name allocation in the intermediate or final tree.

### 3.2 Immutable snapshot

`NodeId` is a `u32` newtype. Root is 0, synthetic metadata/orphan directories follow in that order, then ordinary nodes follow `(record, parent record, raw name, namespace)` order. Internal conversions and array ranges are checked.

| Storage | Fields |
|---|---|
| `#[repr(C)] Node`, in order | Five `u64`: `size`, `alloc`, `total_size`, `total_alloc`, `mtime`; five `u32`: `parent`, `child_start`, `child_len`, `file_count`, `name_off`; then `u16 name_len`, `u16 flags` |
| Parallel cold columns | ADS size, optional FileRef, raw SI attributes, nlink and optional reparse tag |
| Shared arrays | UTF-8 name bytes and CSR `NodeId` children; every non-root node occurs once in child ranges that partition the array |
| Snapshot metadata/indexes | Parsed geometry plus `ScanContext`, top-100 primary files and extension summaries |
| Separate output | `ScanOutput.stats`; checked counters contain neither durations nor transport counts |

The implementation has a compile-time `assert!(size_of::<Node>() == 64)`. Node count, CSR indices and pool length must fit `u32`; individual UTF-8 name lengths must fit `u16`. Overflow is an error. Node flags are directory, reparse, sparse, compressed, hidden, system, hard-link duplicate and synthetic; orphan/metadata placement is represented by the tree.

All snapshot storage is private and frozen when delivered. `Snapshot` is Send + Sync and can be shared through `Arc`; cloning a snapshot copies its owned arrays. `node(NodeId)` returns a read-only `NodeView` covering hot and cold fields. `children` and `name` borrow slices; `path` constructs a string iteratively and fallibly. `len`, `is_empty`, `volume`, `top_files` and `ext_stats` expose the remaining read-only data. Unknown ids return `None`. Dynamic query operations remain future work (§4).

### 3.3 Accounting and final ordering

- `size` is unnamed DATA logical size. `ads_size` sums named DATA logical sizes, excluding `$BadClus:$Bad`. FILE_NAME cached sizes never contribute.
- `alloc` sums every non-resident stream, including DATA, ADS, INDEX_ALLOCATION, BITMAP and security descriptors. Ordinary streams use mapped clusters across all segments × cluster size, including preallocation; sparse/compressed flagged streams use first-segment `compressed_size`. Resident allocation is zero. Holes without an LCN, such as `$BadClus:$Bad`, contribute nothing.
- Iterative BFS followed by reverse accumulation initializes totals from self size/allocation. Primary files contribute one file; directories, synthetic nodes and duplicates contribute zero files. DUP nodes keep self values and totals but contribute nothing to ancestors, top files or extension statistics. Directory self-allocation is included. Logical subtree totals exclude ADS; allocation includes it.
- Children sort by `(-total_alloc, name, FileRef, NodeId)`; top files use `-alloc` with the same ties and include metafiles. Name comparisons re-encode pooled UTF-8 to UTF-16 without allocation. Valid names reproduce raw ordering; malformed names sort by replacement text, then record/sequence and NodeId.
- Extension summaries group primary files by the last ASCII-lowercased suffix without its dot. Leading-dot-only, trailing-dot and no-dot names use the empty extension. Summaries contain count, logical bytes and allocation, sorted by `(-total_alloc, extension)`.

Checked arithmetic protects counters and totals. Output is independent of request chunk size and response arrival order. Root totals describe retained entries, not a reconciliation of all allocated volume clusters.

### 3.4 Snapshot persistence

`scan::ScanOutput::to_bytes` returns an owned byte buffer containing the snapshot and `ScanStats`; `ScanOutput::from_bytes` reconstructs an owned output from a byte slice. Both return `snapshot::PersistError` on failure. `ScanOutput::ADM_MAGIC` exposes the eight-byte signature used by the codec and callers identifying snapshot files. The codec lives in private `snapshot/persist/`, keeping storage access within the snapshot module family.

These convenience methods use the same codec as the bounded file path. `ScanOutput::encoder()` borrows the output and emits chunks of at most 1 MiB, borrowing name pools and labels directly and packing integer columns into reusable scratch. Chunks contain a zero CRC slot; after the last chunk, `SnapshotEncoder::checksum_patch()` exposes its byte offset and final little-endian checksum. `adamite_io::snapshot::SnapshotWriter` writes the chunks and patches the slot with a seek, without a checksum pre-pass or a complete encoded buffer.

The canonical `.adm` v1 format uses explicitly packed little-endian fields. Its 64-byte header contains magic `ADAMITE\0`, version, header length, `file_len`, `section_count`, flags, CRC-32 and reserved bytes. A 13-entry section table records kinds, offsets, lengths and counts for volume metadata, scan counters, nodes, name pools, children, cold columns and indexes. Sections start on 8-byte boundaries with minimal zero padding, and the file ends at the last section's end. Optional FileRef and reparse-tag columns use a presence bitmap followed by an 8-byte-aligned full value array, with absent values and unused bits zero. IEEE CRC-32 covers the whole file with the checksum field treated as zero.

`snapshot::SnapshotDecoder::verify` consumes sequential bytes supplied by the caller, validating the header → section table → placement, padding and section lengths → CRC with 1 MiB scratch before allocating columns fallibly. After the caller rewinds the source, `SnapshotDecoder::decode` consumes a second sequential pass directly into final columns, using exact fallible reservations from the table counts. Presence bits are staged in the final optional columns; extension name lengths are staged in their final strings. `adamite_io::snapshot::SnapshotReader` owns the shared 1 MiB file buffer and both passes. The caller must keep the bytes unchanged between passes.

The second pass decodes values and performs full snapshot invariant validation: geometry and UTF-8, column lengths, root and arena order, hard-link groups, flags, CSR edges and connectivity, subtree aggregates, child ordering, top files and extension summaries. Ranking and extension rules are shared with snapshot finalization. Unknown versions, section kinds and flags are rejected; every accepted encoding re-encodes byte-identically. `ScanStats` counters are preserved without cross-validation against the tree.

## 4. Access Layer (planned)

`add-core-query` will add operations such as summary, directory listing and search over immutable snapshots. Precomputed `top_files` and `ext_stats` are available now; filtering/ranking arbitrary subtrees is not.

The CLI implements saved-snapshot queries and terminal browsing. Budgeted summaries, MCP, a snapshot/job service and USN-based refresh remain planned.

### 4.1 Privilege model

Scanning a Windows raw volume needs elevated device access, while an MCP process spawned by an agent host usually runs unelevated. `adamite_io::reader::RawVolumeReader` owns that access today; a future service will manage privileged scan jobs.

- **Implemented MVP:** run `adamite scan C: -o C.adm` elevated once; the CLI streams the scan through `adamite_io::snapshot::SnapshotWriter`, patches the CRC and publishes the snapshot using a synchronized same-directory temporary file closed before publication. `adamite browse C.adm` and `info`/`top`/`ext`/`ls` read it through the two-pass `adamite_io::snapshot::SnapshotReader` without elevation, given file read access.
- **Later:** an elevated `adamite-daemon` Windows service holds snapshots and keeps them fresh through the USN journal. CLI and MCP become thin clients over a named pipe.

### 4.2 Budgeted summary (planned agent-oriented query)

Agent context is limited, so the planned query returns the most informative tree within a node budget:

```text
summarize(root, budget_nodes, min_share):
  heap = MaxHeap by total_alloc, push(root)
  while emitted < budget_nodes and heap non-empty:
      n = pop; emit(n)
      for c in children(n) (already sorted desc):
          if c.total_alloc / root.total_alloc < min_share: collapse into "… N more (X GB)", break
          push(c)
  render as an indented tree
```

### 4.3 MCP tools (planned)

| Tool | Purpose |
|---|---|
| `scan_volume(volume, profile?)` / `scan_status` / `cancel_scan` | Start a scan or load a snapshot, report progress and snapshot age, cancel a scan |
| `get_tree(path, budget=60, min_share=0.5%)` | Budgeted summary tree, rendered as indented text |
| `list_dir(path, sort, offset, limit)` | Exact paginated directory listing |
| `top_files(n, under?, ext?)` / `top_dirs(n, under?, depth?)` | Largest files / directories |
| `ext_breakdown(under?)` | Count and on-disk size by extension |
| `search(pattern, under?, min_size?)` | Name search by glob/regex |
| `node_info(path)` | Size, alloc, mtime, attributes, hard links, reparse flag |

The planned MCP response convention includes snapshot timestamp and units in every result, with `format: text|json` and text as the default. The implemented CLI mirrors a subset: `scan`, `info`, `top`, `ext` and `ls` use text by default and accept `--json` with the `adamite/1` schema; `browse` opens the TUI. For example, `adamite top C.adm --n 20 --json` reads the stored file index. Budgeted `tree`, search and subtree filtering/ranking await `add-core-query`. See the [CLI README](../crates/adamite-cli/README.md) for the command and output contracts.

## 5. Parallelism and Resource Limits

### 5.1 Current driver limits

Adamite limits its own reads and work through `adamite_io::limits::ScanLimits`. `SyncDriver::with_limits` configures core's chunk size and maximum outstanding requests, validates them through `ScanConfig`, and paces reads between requests. The driver runs on the calling thread, parses serially and reuses one response buffer. Native read depth remains one even when the logical request ceiling is larger. Core starts no threads, reads no clocks and never sleeps.

`SyncDriver::run_observed` calls a synchronous observer after each scanner progress step and completed read batch with `DriverProgress { scan, bytes_read }`; the cumulative logical byte count includes bootstrap and discovery. Batch reports retain the latest scanner state. `run` delegates to this loop with a no-op observer.

The CLI uses these reports for an indicatif display when standard error is a terminal: record percentage and ETA during streaming, phase spinners otherwise, and cleanup before results or errors. Redirected standard error retains the single scan-start line.

```rust
pub struct ScanLimits {
    pub max_read_bytes_per_sec: Option<NonZeroU64>,
    pub io_depth: NonZeroUsize,
    pub chunk_size: usize,
    pub threads: NonZeroUsize,
}

pub enum ScanProfile { Background, Balanced, Max }
```

| Profile | Read budget | Chunk size | io_depth | threads |
|---|---|---|---|---|
| Background | 64 MiB/s | 1 MiB | 1 | 1 |
| Balanced (default) | 256 MiB/s | 1 MiB | 1 | 1 |
| Max | Uncapped | 4 MiB | 1 | 1 |

`threads` is a positive upper bound: the synchronous scan uses one thread for every accepted value. Future parallel work must honour this ceiling. `io_depth` sets `ScanConfig.max_outstanding`; increasing it batches logical requests without adding native read concurrency. Core accepts chunks that are multiples of 512 between 512 bytes and 16 MiB, and rejects an overflowing depth × chunk product. The legacy `SyncDriver::new(ScanConfig)` constructor retains uncapped reads for callers that directly configure core.

Every CLI source command accepts `--profile background|balanced|max`, `--max-read-rate <BYTES>` and `--threads <N>`. Rate suffixes K/M/G and KiB/MiB/GiB use powers of 1024. The rate override replaces the selected profile's budget; zero is rejected, and `--profile max` selects uncapped reads. Snapshot loading ignores scan limits. The development benchmark defaults to Max and accepts `--profile`; its `--chunk-size` override keeps heap comparisons at a fixed request size.

### 5.2 Read pacing and the one-second bound

The I/O-owned token bucket uses integer byte-nanosecond accounting, starts empty and has capacity B = max(C, floor(R / 1000)) bytes, where C is the configured chunk and R is the requested bytes per second. This uses a one-millisecond refill window, with space for at least one chunk. For R greater than B, it replenishes at R − B bytes per second, reserving the bucket capacity from the one-second budget for a possible burst after idle time. For every interval of duration t, admitted request bytes are at most B + (R − B)t. For the named profiles B = C. At t = 1 second the bound is R: Balanced replenishes at 255 MiB/s and admits at most 256 MiB in any one-second window; Background replenishes at 63 MiB/s and admits at most 64 MiB. Integer division rounds waits upward. For budgets at or below one chunk per second, refill uses R and the bound is R × t + C because an indivisible chunk can itself exceed a one-second budget.

Before each read, the driver sleeps for the missing tokens through an I/O-owned clock abstraction. Production uses `Instant` and `thread::sleep`; unit tests inject a clock that advances immediately. Oversized acquisitions consume tokens in capacity-sized pieces so they progress; validated driver requests never exceed the configured chunk. Each sleep emits a `rate_limit_wait` span containing requested bytes and actual `waited_nanos`. The budget counts exact logical bytes requested from `VolumeReader`; Windows alignment can expand native reads to logical sectors.

### 5.3 Future CPU pool and buffer backpressure

Parallel parsing and per-directory sorting will use a driver-owned dedicated rayon pool capped by `limits.threads`. Work and nested parallel iterators must stay in that pool. Core will continue to avoid the global pool. Adamite will enforce its limits through scheduling, pacing and bounded storage; it does not use operating-system priority APIs.

A future asynchronous reader will acquire a buffer from a fixed-size pool before issuing a read, acquire rate tokens, and publish the response through a bounded queue. Feeding the response returns the buffer to the pool. Pool exhaustion stops producers until consumers return buffers. The driver must coordinate pool capacity, native depth and core's outstanding-request ceiling. Today the synchronous driver serves one request at a time and keeps one reusable response buffer; the Windows reader separately reuses its physically aligned native buffer.

The Windows reader opens volumes read-only with `FILE_FLAG_NO_BUFFERING`, expands requests to logical-sector boundaries and copies only the exact requested range. Device opening and volume context remain adapter responsibilities. Cross-extent assembly and virtual ordering remain core responsibilities.

### 5.4 Future service scheduling and memory budgets

A future service scheduler will deduplicate scans by volume and cap concurrent scans, initially at one. Cancellation checks belong before driver reads and between worker chunks. Queries may use a separate small pool. Dedicated pools, parallel parsing/sorting, buffer-pool backpressure, `$MFT:$BITMAP` skipping and service scheduling remain future work.

The retained scaling target is 10M nodes without per-node name allocation. A rough layout estimate is 640 MB of hot nodes, 40 MB of CSR children and approximately 150 MB of names, plus cold columns, scan intermediates and transport storage. Metadata memory and a single directory sort remain outside a general memory budget.

## 6. Implementation Order

1. Done: `adamite-ntfs`, shared fixture support and independently measured v3 manifests; parser `no_std` builds remain supported.
2. Done: single-threaded sans-io `adamite-core`, phases 0–6, fixture oracles, property tests and bounded fuzzing. Automated WizTree/`dir /s` comparison is deferred; manual Windows comparison has not been performed.
3. Done: `adamite-io` with slice/image/Windows raw-volume readers, synchronous driving and adapter-supplied context. Self-imposed read limits and profiles are implemented. Next is roadmap step 3 parallelism, buffer ownership and bitmap skipping, or `add-core-query`.
4. Done: core `.adm` snapshot persistence, the `adamite` CLI, shared display formatting and terminal browser. CI packages Linux/Windows binaries; the tag-triggered release workflow reuses CI artifacts. Future: `add-core-query` for budgeted summaries, search and dynamic subtree queries.
5. Future: MCP and scan-job/snapshot services.
6. Future: USN incremental refresh and a daemon; walkdir fallback remains separate future work.

## 7. Conventions

Rust crates use edition 2024 and toolchain 1.99. Keep `rust-toolchain.toml` and the workspace `rust-version` synchronized. Core forbids unsafe code; `adamite-io` denies it except in `reader/windows/ffi.rs`, which denies `clippy::undocumented_unsafe_blocks` and `clippy::multiple_unsafe_ops_per_block`. Non-Windows `adamite-io` builds forbid unsafe code. Both crates adopt the parser's panic-free Clippy denies; grow owned scan buffers fallibly and use checked arithmetic. Do not recurse through file trees.

Production types must not carry fields, variants or hook state used only by tests; tests observe real outputs or existing production state.

Library errors use snafu with source chains and documented `# Errors` sections. Core separates configuration, feed, fatal scan, snapshot and persistence errors; ordinary rejected records are counted in `ScanStats`. Workspace snafu disables default features and enables `rust_1_81`, preserving parser `no_std` support.

Crates that log use workspace tracing: core emits per-call phase spans and bootstrap/ingest/merge/link/build/aggregate/finalize stage spans; persistence emits encode/decode/validate spans; I/O emits batch, reader and rate-limit-wait spans; the CLI emits source/load/scan/save/render spans; the TUI emits startup and frame-render spans. Stage handles may span driver calls, but entered guards never survive a driver call. The parser does not log. The `adamite` binary installs a tracing subscriber through `adamite-profile` only when `--trace-chrome <FILE>` is given. Its counting allocator tracks requested heap bytes; each span closes with a `heap_usage` event containing `heap_current` (current live bytes), `heap_peak` (the maximum live bytes while that span was open), `heap_delta` (the signed change since span creation) and `process_peak` (the process-wide maximum). At each span open/close boundary, the heap layer folds the allocator interval peak into every open span before adding or removing that span, preserving peaks for nested and overlapping lifetimes. The trace guard finishes the JSON file when the command returns, including errors. Adapters read the clock and supply timestamps through caller context; core reads no clocks.

Task automation and fixture generation use Deno TypeScript through root `deno.json`. `deno task check` includes `adamite-io`, `adamite-cli`, `adamite-tui`, `adamite-profile` and `adamite-bench` Clippy for `x86_64-pc-windows-gnu`; native Windows CI also runs the ignored elevated I/O and CLI raw-volume tests after workspace tests. Do not add bash, Python or just scripts; CI steps invoke only `cargo` or `deno` one-liners. The separate `deno task bench` runs release resource measurements for 300k and 1M in-use records through the shared profiling recorder. Plans live in `docs/plans/`, one file per change.
