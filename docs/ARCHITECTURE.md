# Adamite Architecture

Adamite aims to give agents a WizTree-like view of a filesystem through MCP, CLI and other front ends. The implemented libraries parse caller-supplied NTFS Master File Table (MFT) bytes and produce an immutable, size-aggregated `Snapshot` with `ScanStats`. Platform readers and front ends are planned.

The project is split into two layers:

- **Core layer:** scanning and tree building, with query and persistence modules planned.
- **Access layer (planned):** MCP, CLI and other front ends, backed by snapshot and scan-job services.

Implementation language: Rust (Cargo workspace).

## 1. Overall Architecture

The diagram includes planned components; only the parser, single-threaded scan/snapshot implementation and fixture support exist today.

```text
┌──────────────────── Access layer (planned) ────────────────────┐
│ adamite-cli       adamite-mcp (stdio/HTTP)      HTTP/gRPC        │
│      └──────────────────┬────────────────────────┘             │
│                         ▼                                      │
│ adamite-service: snapshot cache / scan jobs / staleness /       │
│                  privilege management                          │
└────────────┬───────────────────────────────┬────────────────────┘
             │ Snapshot (immutable, Arc)     │ drive scans
             │ + planned Query API          ▼
             │          ┌── Caller driver + I/O (planned) ───────┐
             │          │ adamite-io::VolumeReader               │
             │          │ Windows volume / raw image / Linux     │
             │          │ block device; aligned buffers and reads│
             │          └────────────┬─────────────────▲──────────┘
             │             step()/feed(id, bytes)      │ Need(id,
             │                       │                 │ offset,len)
┌────────────▼───────────────────────▼─────────────────┴──────────┐
│ Core layer                                                     │
│ adamite-core                                                   │
│   scan/: boot → discover MFT → stream → merge                   │
│   tree/ + snapshot/: link → arena/CSR/name pool → aggregate     │
│                      → finalize → Done(snapshot, stats)        │
│   query/ (planned): summary / listing / search / subtree ranks │
│   persist/ (planned): flat-array snapshot serialization        │
│ adamite-ntfs: boot/record/attribute/value/utf16 decoding         │
└────────────────────────────────────────────────────────────────┘
```

### Workspace layout

```text
crates/
  adamite-ntfs/     # implemented: boot/record/attribute/value/utf16, no_std
  adamite-core/     # implemented: scan/tree/snapshot; query/persist planned
  adamite-fixtures/ # implemented: shared development-only fixture support
  adamite-io/       # planned: VolumeReader + platform adapters serving requests
  adamite-service/  # planned: snapshots, jobs, privilege and resource scheduling
  adamite-cli/      # planned
  adamite-mcp/      # planned
```

| Implemented crate | Responsibility |
|---|---|
| `adamite-ntfs` | `no_std` byte decoding through `boot`, `record`, `attribute`, `value` and `utf16` modules; record parsing owns fixup, and attribute runlists live under `attribute::runlist` |
| `adamite-core` | Single-threaded `std` scan driver, NTFS merge, generic linking, aggregation and snapshot storage |
| `adamite-fixtures` | Unpublished development support: image loading, independent manifests/reference MFT reconstruction and synthetic builders |

Parser operations belong to their types: `boot::VolumeGeometry::from_boot_sector` decodes volume geometry, `record::FileRecord::parse` validates headers and restores protected trailers in place, and `record::RecordClass::classify` applies streaming classification precedence. Parsing errors leave record bytes unchanged. Typed attribute decoders live under `value`; record references and system record numbers live under `record::reference` and `record::system`. Every public parser item has one module path.

Core performs no file, device or network I/O. It defines no reader trait or callback, starts no threads, reads no clocks and never sleeps. The caller owns transport and scheduling; `ScanContext` supplies optional label, scan FILETIME and free bytes. Core never acquires these values itself.

`EntryRecord` separates NTFS ingestion/merge from generic tree processing. A future fallback scanner could use this boundary, but no walkdir scanner exists yet. Future I/O, query, persistence, service, CLI, MCP and USN components are described in §4–§6.

### Key decisions

| Decision | Rationale |
|---|---|
| `adamite-ntfs` and `adamite-core` do no I/O | Caller-owned reads allow the same parsing/scan code to use memory images, platform adapters and fuzz inputs. |
| MFT mapping comes from record 0's own runlist and attribute-list discovery, not `FSCTL_GET_RETRIEVAL_POINTERS` | Live volumes and images can use the same bootstrap algorithm. |
| Snapshot is immutable and can be shared through `Arc` | Multiple consumers can read it without locking; cloning Snapshot itself copies its arrays. |
| Scanning is decoupled from queries; persistence is planned | Future raw-volume scans can run elevated while saved-snapshot queries run unelevated (§4.1). |
| A planned walkdir fallback emits the same EntryRecord boundary | FAT/exFAT/ReFS/network drives could reuse tree building without an NTFS parser. |

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

Attributes and required runlists are exhausted before publishing a record. Ordinary parse failures discard its entire contribution and increment diagnostic counters; deleted/BAAD/unused records produce no nodes. Invalid geometry/bootstrap, a missing or corrupt directory root, arithmetic/representation overflow and allocation failure terminate the scan. Reparse tags are retained; targets are never followed.

Linking consumes only `EntryRecord`. It deduplicates identical links and drops DOS namespace 2 names when the same parent FileRef has a non-DOS name; DOS-only names remain. Distinct non-DOS names, including same-parent hard links, become separate nodes. Primary selection uses `(parent record, raw UTF-16 name, namespace, parent sequence)` ascending; secondary nodes have `HARDLINK_DUP`.

Record 5 is required and becomes NodeId 0 named `/`; its self-parent name creates no edge. Parents must be surviving directory entries with matching sequences. Children of a multi-name directory attach to its primary node. Unresolved links and every cycle-member directory are reparented under `<orphaned>`; descendants stay beneath their directories. Cycle detection is iterative.

In-use bases 0–15 except root and direct children of `$Extend` are grouped under `<NTFS metadata>`; descendants remain beneath them. Synthetic directories are created only when needed and have zero self sizes and no FileRef. Nameless records receive `<record N>` names. A live-volume scan is not atomic; coherent source bytes are the caller's responsibility, and no USN checkpoint is acquired.

## 3. Data Structures

### 3.1 Scan-time ownership

`Vec<RecordSlot>` grows in ordinal order and holds classification, optional sequence/record flags and an optional owned base contribution. Contributions retain raw UTF-16 names, SI metadata, reparse tags and stream segments. Extension `Patch` values carry base and source FileRefs plus their owned contribution, including attribute instance ids; extensions may precede bases because merge follows the full stream pass.

A stream key is `(attribute type, raw UTF-16 name)`. Segment identity adds lowest VCN, source FileRef and attribute id. Only `lowest_vcn == 0` supplies sizes. Duplicate resident streams, conflicting first segments, gaps and overlaps discard the affected ordinary base. Missing/stale-base patches are rejected without modifying the base. Merge produces the scalar sizes, flags, metadata and names in `EntryRecord`.

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
- `alloc` sums every non-resident stream, including DATA, ADS, INDEX_ALLOCATION, BITMAP and security descriptors, once from its first segment. Compressed/sparse streams use `compressed_size`; other streams use `allocated_size`. Resident allocation is zero.
- Iterative BFS followed by reverse accumulation initializes totals from self size/allocation. Primary files contribute one file; directories, synthetic nodes and duplicates contribute zero files. DUP nodes keep self values and totals but contribute nothing to ancestors, top files or extension statistics. Directory self-allocation is included. Logical subtree totals exclude ADS; allocation includes it.
- Children sort by `(-total_alloc, name, FileRef, NodeId)`; top files use `-alloc` with the same ties and include metafiles. Name comparisons re-encode pooled UTF-8 to UTF-16 without allocation. Valid names reproduce raw ordering; malformed names sort by replacement text, then record/sequence and NodeId.
- Extension summaries group primary files by the last ASCII-lowercased suffix without its dot. Leading-dot-only, trailing-dot and no-dot names use the empty extension. Summaries contain count, logical bytes and allocation, sorted by `(-total_alloc, extension)`.

Checked arithmetic protects counters and totals. Output is independent of request chunk size and response arrival order. Root totals describe retained entries, not a reconciliation of all allocated volume clusters.

## 4. Access Layer (planned)

`add-core-query` will add operations such as summary, directory listing and search over immutable snapshots. Precomputed `top_files` and `ext_stats` are available now; filtering/ranking arbitrary subtrees is not.

The following privilege model, algorithms and tools are planned. Snapshot persistence, CLI, MCP, a snapshot/job service and USN-based refresh are unimplemented. Planned persistence uses a header and flat-array sections; mmap-friendly encoding or `rkyv` are candidates, with no serialized format or daemon protocol established yet.

### 4.1 Privilege model

Scanning a Windows raw volume needs elevated device access, while an MCP process spawned by an agent host usually runs unelevated. The future I/O adapter or service owns that access.

- **Planned MVP:** run `adamite scan C: -o C.adm` elevated once to produce a snapshot file. MCP and CLI queries read the `.adm` file without elevation.
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

The planned response convention includes snapshot timestamp and units in every result, with `format: text|json` and text as the default to save tokens. CLI commands will mirror the tools, for example `adamite tree C:\Users --budget 80` and `adamite top --n 20 --json`.

## 5. Parallelism and Resource Limits (planned)

`adamite-io`/`add-volume-reader` is the next change. Any `VolumeReader` abstraction belongs there. Adapters will open devices/images, perform reads, propagate host errors and feed the exact requested bytes to core. Native alignment belongs to the adapter: it may round device reads outward into aligned buffers and slice the requested range, including core's 512-byte boot request on a 4Kn source.

The full design below is future roadmap step 3 work. CPU, I/O, transport memory and concurrent scans each have their own cap, coordinated by a driver/service-level `ScanLimits`. Core already supports multiple outstanding requests through `ScanConfig.max_outstanding`; it still processes records on one thread. Buffer pools, native I/O depth, rate limiting and waiting belong to the adapter/driver, and core never uses a global thread pool.

### 5.1 Resources and controls

| Resource | Failure if unbounded | Planned control |
|---|---|---|
| CPU | Parse workers and sorting saturate all cores and the foreground stutters | A driver-owned dedicated rayon ThreadPool with a capped thread count and lowered priority |
| Disk I/O | Large sequential reads fill the disk queue and increase other I/O latency | Adapter cap on in-flight native reads, optional rate limit and low I/O priority |
| Transport memory | The reader outpaces consumption and buffers pile up | Fixed-size adapter buffer pool and bounded response queue, coordinated with core's request cap |
| Concurrent scans | Several agents trigger scans and their loads stack | Service semaphore; one scan per volume, with duplicate requests sharing the running task |

The retained scaling target is 10M nodes without per-node name allocation. A rough layout estimate is 640 MB of hot nodes, 40 MB of CSR children and approximately 150 MB of names, plus cold columns, scan intermediates and transport storage. This is a planning estimate, not a measured total-memory bound; metadata budgets remain future work.

### 5.2 Configuration

This proposed configuration belongs to the future adapter/scan driver; its profile defaults are separate from today's core defaults of cap 1 and 1 MiB chunks.

```rust
pub struct ScanLimits {
    /// Future parse/sort worker count. Planned default: max(1, cores / 2).
    pub threads: NonZeroUsize,
    /// Planned driver chunk size: 4 MiB; also configure ScanConfig.chunk_size.
    pub chunk_size: usize,
    /// Adapter buffers in flight or queued. Planned default: threads + 2.
    pub buffers: NonZeroUsize,
    /// Outstanding native reads. Planned default: 2; 1 for synchronous I/O.
    pub io_depth: NonZeroUsize,
    /// Optional adapter token-bucket throughput cap. Planned default: None.
    pub max_read_bytes_per_sec: Option<NonZeroU64>,
    /// Lower driver/worker CPU and I/O priority. Planned default: true.
    pub low_priority: bool,
}

pub enum ScanProfile { Background, Balanced, Max }
```

| Planned profile | threads | io_depth | Rate limit | Low priority |
|---|---|---|---|---|
| Background | 2 | 1 | 200 MB/s | yes |
| Balanced (default) | max(1, cores / 2) | 2 | none | yes |
| Max | all cores | 4 | none | no |

The planned CLI permits field overrides such as `--threads 4 --io-depth 1 --max-read 300M`; MCP `scan_volume` accepts a profile plus overrides. The driver coordinates native `io_depth`, buffer capacity and core's `max_outstanding`: native reads and unconsumed logical requests are different limits. Raising the request cap does not parallelize core.

### 5.3 CPU: dedicated pool

The future driver/service owns the CPU pool. A later parallel implementation will route parse and sort work onto it; the current Scanner remains serial.

```rust
let pool = rayon::ThreadPoolBuilder::new()
    .num_threads(limits.threads.get())
    .thread_name(|i| format!("adamite-scan-{i}"))
    .start_handler(move |_| if low_priority { platform::lower_thread_priority() })
    .build()?;

pool.install(|| {
    // Planned Phase 1 parsing and Phase 6 sorting/extension-stat work.
    // Nested parallel iterators stay on this dedicated pool.
});
```

- Planned parallel work uses the supplied pool rather than `rayon::spawn` or a global pool, so embedding applications keep control of their threads.
- The proposed blocking reader is one extra driver-owned, mostly I/O-blocked thread and does not count toward `threads`.
- On Windows, the driver will lower worker priority with `SetThreadPriority(THREAD_PRIORITY_BELOW_NORMAL)`. A standalone CLI may use `PROCESS_MODE_BACKGROUND_BEGIN` to lower CPU, I/O and memory-page priority together.

### 5.4 I/O and memory: buffer-pool backpressure

The planned adapter/driver pipeline is:

```text
          ┌──── free buffers (bounded, N = buffers) ◄───────────────────┐
          ▼                                                            │
adapter: take buffer → rate limit → aligned read → bounded ready queue  │
                                                  │                   │
driver:                               feed(id, exact slice) → return buffer
                                                  │ copies response
                                                  ▼
core:                                  step() → consume request slot
```

- Allocate the adapter's aligned buffers once and reuse them. Pool payload capacity is approximately `chunk_size × buffers`, with alignment padding; core's copied responses and growing metadata require additional memory.
- The reader must acquire a free buffer before issuing a read. A full response queue or exhausted pool blocks the driver/reader. Core's occupied request slots independently cap accepted but unconsumed responses; the driver also stops issuing work when that cap is reached.
- After a successful `feed`, core retains its own copy, so the adapter can return the source buffer to the pool immediately. Core neither borrows the pool nor waits on its channels.
- On Windows, the planned raw-volume adapter uses `FILE_FLAG_NO_BUFFERING` with sector-aligned offsets, lengths and buffers. It rounds outward as needed and feeds only the exact requested range.
- `io_depth` caps outstanding native reads, with 1 for synchronous reads. Depth 2 is the proposed balanced starting point for sequential HDD access, subject to measurement on the device.
- The planned rate limiter is a token bucket: acquire `chunk_len` before reading and sleep in the adapter/driver when tokens run out.
- Low I/O priority is planned through `SetFileInformationByHandle(FileIoPriorityHintInfo, IoPriorityHintLow)` on the volume handle.

### 5.5 Service-level scan scheduling

The proposed service scheduler is:

```rust
pub struct ScanScheduler {
    global: Semaphore,                                  // concurrent scans, default 1
    running: Mutex<HashMap<VolumeId, Shared<ScanTask>>>,  // per-volume deduplication
}
```

- A duplicate request for a volume will return the running task's handle; callers subscribe to its progress.
- The planned global semaphore defaults to 1. Different-volume scans queue and run serially so their worker pools do not stack beyond configured limits.
- Each future scan job carries a CancellationToken. The driver checks it before reads and driver calls, and future workers check it per chunk. Dropping the current Scanner stops further core work; no cancellation-token API exists in core. The planned cancellation target is tens of milliseconds, to be validated against directory-sort latency; MCP exposes `cancel_scan`.
- Queries are planned as mostly single-threaded slice operations. Full-scan operations such as search may use a small shared query pool, initially 2 threads.

### 5.6 Impact on the pipeline

- Phase 0 device opening, aligned reads and future USN-position acquisition belong to the adapter/service. Core keeps its boot → record 0 → discovery protocol.
- Phase 1 will use adapter buffer-pool backpressure and, in the later parallel design, capped parse workers. Cross-extent assembly and virtual ordering remain core responsibilities.
- Phases 3–5 (link, CSR construction and aggregation) remain single-threaded linear passes in the proposed parallel pipeline.
- Phase 6 is planned to use parallel per-directory sorting and extension-stat work on the same dedicated CPU pool.
- Future driver/service telemetry will record `threads_used`, `peak_buffer_bytes` and `throttled_ms` so resource controls can be measured. Core ScanStats remains independent of transport counts and clocks.

Dedicated CPU pools, parallel parse/sort, buffer-pool backpressure, `$BITMAP` skipping and general CPU/memory/scan limits remain roadmap step 3 work. No ScanLimits, resource profiles, global pool or service scheduler is implemented. Memory for metadata and a single directory sort's cost remain unbounded by a general budget.

## 6. Implementation Order

1. Done: `adamite-ntfs`, shared fixture support and independently measured v3 manifests; parser `no_std` builds remain supported.
2. Done: single-threaded sans-io `adamite-core`, phases 0–6, fixture oracles, property tests and bounded fuzzing. Automated WizTree/`dir /s` comparison is deferred; manual Windows comparison has not been performed.
3. Next: `adamite-io` in `add-volume-reader`; then roadmap step 3 parallelism, buffer ownership, bitmap skipping and resource controls.
4. Future: `add-core-query`, snapshot persistence and CLI.
5. Future: MCP and scan-job/snapshot services.
6. Future: USN incremental refresh and a daemon; walkdir fallback remains separate future work.

## 7. Conventions

Rust crates use edition 2024 and toolchain 1.99. Keep `rust-toolchain.toml` and the workspace `rust-version` synchronized. Core forbids unsafe code and adopts the parser's panic-free Clippy denies; grow owned scan buffers fallibly and use checked arithmetic. Do not recurse through file trees.

Production types must not carry fields, variants or hook state used only by tests; tests observe real outputs or existing production state.

Library errors use snafu with source chains and documented `# Errors` sections. Core separates configuration, feed, fatal scan and snapshot errors; ordinary rejected records are counted in `ScanStats`. Workspace snafu disables default features and enables `rust_1_81`, preserving parser `no_std` support.

Crates that log use workspace tracing, beginning with core's per-call phase spans; entered guards never survive a driver call. The parser does not log. Future binaries will install subscribers. Core reads no clocks; caller context supplies timestamps.

Task automation and fixture generation use Deno TypeScript through root `deno.json`. Do not add bash, Python or just scripts; CI steps invoke only `cargo` or `deno` one-liners. Plans live in `docs/plans/`, one file per change.
