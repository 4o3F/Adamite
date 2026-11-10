# Adamite Architecture

Adamite gives agents a WizTree-like view of a filesystem. It reads the NTFS Master File Table (MFT) directly, builds a size-aggregated file tree, and exposes it through MCP, CLI and other front ends.

The project is split into two layers:

- **Core layer**: scanning and tree building.
- **Access layer**: MCP, CLI and future front ends.

Implementation language: Rust (Cargo workspace).

---

## 1. Overall Architecture

```
┌───────────────────────────── Access layer ────────────────────────────────┐
│  adamite-cli            adamite-mcp (stdio/HTTP)        (future) HTTP/gRPC │
│     │                        │                                             │
│     └──────────┬─────────────┘                                             │
│                ▼                                                           │
│        adamite-service  ── snapshot cache / scan jobs / staleness / privilege │
└────────────────┬───────────────────────────────────────────────────────────┘
                 │  Snapshot (immutable, Arc)  +  Query API
┌────────────────▼──────────────────────── Core layer ───────────────────────┐
│  adamite-core                                                              │
│   ├─ scan/      pipeline: read MFT → parallel parse → merge → link → aggregate │
│   ├─ tree/      flat arena + CSR child index + name pool                   │
│   ├─ query/     children / path / top-N / ext stats / search / budgeted summary │
│   └─ persist/   snapshot serialization (flat arrays, mmap-able)            │
│  adamite-ntfs   pure parsing, no I/O: boot sector, FILE record, fixup,     │
│                 attributes, runlist                                        │
│  adamite-io     trait VolumeReader: Windows volume / raw image / Linux block device │
└────────────────────────────────────────────────────────────────────────────┘
```

### Workspace layout

```
crates/
  adamite-ntfs/     # depends only on &[u8]; fuzzable; testable on Linux
  adamite-io/       # VolumeReader + platform implementations
  adamite-core/     # scan pipeline, tree, query, persistence
  adamite-service/  # snapshot management, job scheduling, resource limits
  adamite-cli/
  adamite-mcp/
```

### Key decisions

| Decision | Rationale |
|---|---|
| `adamite-ntfs` does no I/O | Unit tests, fuzzing and benchmarks run on Linux/WSL against dd images or exported `$MFT` files |
| MFT location comes from record 0's own runlist, not `FSCTL_GET_RETRIEVAL_POINTERS` | The same code path works for live volumes and images |
| The Snapshot is immutable and shared through `Arc` | Queries need no locks, so multiple agents can query concurrently |
| Scanning is decoupled from querying, and snapshots can be persisted | Scanning needs admin rights; querying does not (see §4.1) |
| A fallback `walkdir` scanner emits the same `EntryRecord`s | Supports FAT/exFAT/ReFS/network drives without changing tree building |

---

## 2. Scan Pipeline

### 2.1 Overview

```
Phase 0  Open & Geometry
   open \\.\C: (admin) → boot sector → MFT record 0
   → $MFT $DATA runlist = MFT extents; $MFT $BITMAP = in-use record bitmap
   → (Windows) record current USN journal position for incremental refresh
Phase 1  Read + Parse (parallel, bounded — see §5)
   reader thread: split MFT virtual offset space into 4–16 MiB chunks, map extents → LCN, sequential reads
   worker pool: per chunk → fixup each record → parse attributes
      → write slots[record_no] (disjoint slots, lock-free)
      → extension records emit Patch into thread-local Vec
Phase 2  Merge
   apply extension-record Patches to their base records
Phase 3  Link
   for each in-use base record: resolve parent_ref in slots, validate seq
   → one edge per hard link; unresolved parents go under <orphaned>
Phase 4  Build CSR
   count children per parent → prefix sum → fill child_index
Phase 5  Aggregate
   BFS from root (FRN 5), iterate BFS order in reverse accumulating into parents (no recursion)
Phase 6  Finalize (parallel)
   sort each directory's children by total_alloc desc; build top-N files and extension stats
   → Snapshot
```

### 2.2 Reading

- **Boot sector** (offset 0): `bytes_per_sector` is a u16 at 0x0B; `sectors_per_cluster` is a u8 at 0x0D; `total_sectors`, `mft_lcn`, `mftmirr_lcn` and the volume serial are u64 fields at 0x28, 0x30, 0x38 and 0x48. The cluster byte is a literal power of two from 1 to 128, or an exponent byte 0xF4..=0xFF encoding 2^(256 − value) sectors, with a maximum cluster size of 2 MiB. File and index record sizes use signed bytes at 0x40 and 0x44: positive n means n clusters; negative −k means 2^k bytes. File records are usually 1024 bytes, or 4096 on 4Kn volumes.
- **The MFT itself may be fragmented.** Read record 0 (`$MFT`) and decode its non-resident `$DATA` runlist into `Vec<Extent { vcn, lcn, len }>`.
- **`$MFT`'s `$BITMAP` attribute** marks which records are in use. Chunk planning skips large unused regions.
- **Chunks are defined in MFT virtual offset space.** The reader stitches extents into a contiguous buffer, which handles records that straddle an extent boundary (e.g. 512 B clusters with 1 KiB records).
- **On Windows, use `FILE_FLAG_NO_BUFFERING`** with sector-aligned offsets and buffers, rotated through a fixed buffer pool (see §5).

### 2.3 Parsing a FILE record

```
Record header
 0x00 "FILE"          0x04 usa_offset u16   0x06 usa_count u16
 0x08 lsn u64
 0x10 sequence u16    0x12 link_count u16   0x14 first_attr u16
 0x16 flags u16 (0x1 IN_USE, 0x2 DIRECTORY)
 0x18 bytes_used u32  0x1C bytes_allocated u32
 0x20 base_ref u64 (≠0 → extension record)  0x28 next_attr_id u16
 NTFS 3.1: usa_offset ≥ 0x30, record_no u32 at 0x2C
 NTFS 3.0: USA starts at 0x2A, record_no is absent
```

1. **Peek at the header and check `IN_USE`.** Validate the header, then skip records with `IN_USE` clear before applying fixup.
2. **Apply fixup to in-use records.** Protection always uses 512-byte strides, including on 4Kn volumes. Validate all USA metadata and every trailer against `usa[0]` before restoring any trailer with its USA entry. A mismatch means a torn write: count it in `stats.bad_fixup` and skip the record with its buffer unchanged.
3. Walk the attributes until `0xFFFFFFFF`. The attributes we need:

| Type | Attribute | What we take |
|---|---|---|
| 0x10 | `$STANDARD_INFORMATION` | mtime and file attributes (hidden/system/compressed/sparse/reparse) |
| 0x30 | `$FILE_NAME` | parent_ref (48-bit record + 16-bit seq), UTF-16 name, namespace (0 POSIX / 1 Win32 / 2 DOS / 3 Win32&DOS) |
| 0x80 | `$DATA` | Unnamed = main stream; named = ADS. Resident: size = value_len, alloc = 0 (data lives inside the MFT). Non-resident: `data_size@0x30`, `alloc@0x28`; when compressed/sparse, use `compressed_size@0x40` as the real on-disk size |
| 0xA0 | `$INDEX_ALLOCATION` | Counted as the directory's own alloc (matches WizTree) |
| 0xC0 | `$REPARSE_POINT` | Flag only. The tree is built from parent refs, so junctions/symlinks are never followed or double-counted |

Attribute header:

```
0x00 type u32  0x04 length u32  0x08 non_resident u8  0x09 name_len u8  0x0A name_off u16
0x0C flags u16 (0x0001 compressed, 0x4000 encrypted, 0x8000 sparse)  0x0E id u16
resident:     0x10 value_len u32  0x14 value_off u16
non-resident: 0x10 lowest_vcn i64  0x18 highest_vcn i64  0x20 runlist_off u16
              0x22 compression_unit u8  0x23..0x28 reserved (5 bytes)
              0x28 allocated_size  0x30 data_size  0x38 initialized_size
              0x40 compressed_size (compressed/sparse header of at least 0x48 bytes)
```

Runlist encoding: the header byte's low nibble is the length field size and its high nibble is the offset field size. Run lengths must be positive. The offset is a signed delta from the previous LCN, whose base resets to zero for every segment; an offset size of 0 means a sparse run that keeps the previous physical LCN base.

### 2.4 Pitfalls

- **Sizes inside `$FILE_NAME` are stale** (they are only updated on rename). Always use `$DATA`.
- **Non-resident streams may span multiple extension records.** All size fields, including allocated, data, initialized and compressed sizes, are valid only in the segment with `lowest_vcn == 0`.
- **`$ATTRIBUTE_LIST` supports `$MFT` bootstrap.** Phase 0 uses its entries to locate DATA segments in extension records reachable through already-known MFT extents. Phase 1 scans extension records in full and merges them into their base by `base_ref`.
- **Root parent:** the root directory's reference and parent reference are both 5:5.
- **Directory record flags:** use DIRECTORY (0x2) to identify directories and preserve 0x4 and 0x8 as raw flags.
- **DOS short names are deduplicated.** Within one parent, prefer Win32/POSIX, then Win32&DOS; use the DOS name only as a last resort.
- **Hard links:** each `$FILE_NAME` under a different parent becomes its own edge. Bytes are counted only on the primary link; other links are flagged `HARDLINK_DUP` and count as 0 during aggregation (configurable).
- **Metafiles:** records 0–15 (`$MFT`, `$LogFile`, `$Bitmap`, …) and entries under `$Extend` go under a virtual `<NTFS metadata>` node by default, so the volume total matches "used space".
- **Orphans:** if the parent's sequence number does not match the one in parent_ref, the parent was deleted or its record reused. The node goes under a synthetic `<orphaned>` node.
- **Consistency:** a live volume changes during the scan, so the MFT read is not atomic. Small inconsistencies are accepted; the USN recorded at scan start marks the snapshot point.

---

## 3. Data Structures

Target: 10M entries in memory with no per-node heap allocation. Everything is a flat SoA-style arena indexed by `u32`.

### 3.1 Scan-time intermediate structures

```rust
/// Dense, indexed by MFT record number. Workers write disjoint slots.
pub struct RecordSlot {
    pub seq: u16,
    pub flags: SlotFlags,          // IN_USE | DIR | REPARSE | COMPRESSED | SPARSE | METAFILE
    pub attrs: u32,                // FILE_ATTRIBUTE_* from $STANDARD_INFORMATION
    pub mtime: u64,                // FILETIME
    pub size: u64,                 // unnamed $DATA logical size
    pub alloc: u64,                // on-disk bytes (incl. ADS + $INDEX_ALLOCATION)
    pub ads_size: u64,
    pub names: NameList,           // usually 1, inline small-vec
}

pub struct NameLink {
    pub parent: FileRef,           // 48-bit record + 16-bit seq
    pub name: NameRef,             // into the per-worker name pool, remapped at merge
    pub namespace: u8,
}

/// Produced by extension records, merged into base in Phase 2.
pub enum Patch {
    Data   { base: FileRef, size: Option<u64>, alloc: Option<u64>, named: bool },
    Name   { base: FileRef, link: NameLink },
    Index  { base: FileRef, alloc: Option<u64> },
}

#[derive(Clone, Copy)]
pub struct FileRef(u64);           // record() -> u64 (low 48), sequence() -> u16 (high 16)
```

`slots` is a `Vec<RecordSlot>` with length `mft_size / record_size`, so FRN-to-slot lookup is plain array indexing with no HashMap.

### 3.2 Final Snapshot (immutable)

```rust
pub type NodeId = u32;

#[repr(C)]
pub struct Node {                  // 64 bytes
    pub size: u64,                 // self logical
    pub alloc: u64,                // self on-disk
    pub total_size: u64,           // subtree logical
    pub total_alloc: u64,          // subtree on-disk
    pub mtime: u64,
    pub parent: NodeId,
    pub child_start: u32,          // range into Snapshot::children
    pub child_len: u32,
    pub file_count: u32,           // files in subtree
    pub name: NameRef,             // { off: u32, len: u16 } into name pool (UTF-8)
    pub flags: NodeFlags,          // u16: DIR | HIDDEN | SYSTEM | REPARSE | HARDLINK_DUP | ORPHAN | META | SPARSE | COMPRESSED
}

pub struct Snapshot {
    pub volume: VolumeInfo,        // letter, serial, cluster size, total/free, scanned_at, usn_checkpoint
    pub nodes: Vec<Node>,          // nodes[0] = volume root
    pub children: Vec<NodeId>,     // CSR: children of n = children[n.child_start..][..n.child_len], sorted by total_alloc desc
    pub names: Vec<u8>,            // UTF-8 name pool
    pub frn: Vec<u64>,             // NodeId -> FileRef (cold, for USN incremental update)
    pub top_files: Vec<NodeId>,    // precomputed top-K by alloc
    pub ext_stats: Vec<ExtStat>,   // { ext: NameRef, count, total_alloc }, sorted desc
    pub stats: ScanStats,          // records_total/in_use/bad_fixup/orphans/hardlinks, durations, limits used
}
```

Rationale:

- **CSR instead of first_child/next_sibling.** Siblings sit contiguously in memory, sorting parallelizes per directory, and paginated listing (`offset/limit`) is an O(1) slice.
- **UTF-8 name pool.** Conversion happens in parallel during Phase 1, and ASCII names take half the space of UTF-16.
- **Hot data stays in `nodes`;** cold columns such as `frn` are stored separately.
- **Estimate:** 10M nodes ≈ 640 MB nodes + 40 MB children + ~150 MB names. If needed, `mtime` and `size` can move to cold columns.
- **Persistence:** the snapshot is a handful of flat arrays, so the file is a header plus raw slices and can later be mmapped zero-copy (or use `rkyv`).

### 3.3 Aggregation (Phase 5)

```rust
// BFS order from root; reverse iteration guarantees children before parents.
let order = bfs_order(&nodes, &children, ROOT);
for &id in order.iter().rev() {
    let n = &nodes[id as usize];
    let (s, a, f) = if n.flags.contains(HARDLINK_DUP) { (0, 0, 0) }
                    else { (n.total_size, n.total_alloc, n.file_count) };
    let p = n.parent as usize;
    if id != ROOT { nodes[p].total_size += s; nodes[p].total_alloc += a; nodes[p].file_count += f; }
}
```

Initialization: `total_* = self`; files have `file_count = 1`. There is no recursion, so deep directory nesting cannot overflow the stack.

---

## 4. Access Layer

### 4.1 Privilege model

Scanning a raw volume needs admin rights, but an MCP process spawned by an agent host usually runs unelevated.

- **MVP:** run `adamite scan C: -o C.adm` elevated once to produce a snapshot file. MCP and CLI queries read the `.adm` file and need no elevation.
- **Later:** an elevated `adamite-daemon` (Windows service) holds snapshots and keeps them fresh through the USN journal. CLI and MCP become thin clients over a named pipe.

### 4.2 Budgeted summary (agent-oriented core query)

Agent context is limited, so the core provides "the most informative tree within N nodes":

```
summarize(root, budget_nodes, min_share):
  heap = MaxHeap by total_alloc, push(root)
  while emitted < budget and heap non-empty:
      n = pop; emit(n)
      for c in children(n) (already sorted desc):
          if c.total_alloc / root.total_alloc < min_share: collapse into "… N more (X GB)", break
          push(c)
  render as an indented tree
```

### 4.3 MCP tools

| Tool | Purpose |
|---|---|
| `scan_volume(volume, profile?)` / `scan_status` / `cancel_scan` | Start a scan or load a snapshot, report progress and snapshot age, cancel a scan |
| `get_tree(path, budget=60, min_share=0.5%)` | Budgeted summary tree, rendered as indented text |
| `list_dir(path, sort, offset, limit)` | Exact paginated directory listing |
| `top_files(n, under?, ext?)` / `top_dirs(n, under?, depth?)` | Largest files / directories |
| `ext_breakdown(under?)` | Count and on-disk size by extension |
| `search(pattern, under?, min_size?)` | Name search by glob/regex |
| `node_info(path)` | Size, alloc, mtime, attributes, hard links, reparse flag |

Every result includes the snapshot timestamp and units, and supports `format: text|json` (text by default, to save tokens). The CLI mirrors these tools: `adamite tree C:\Users --budget 80`, `adamite top --n 20 --json`, and so on.

---

## 5. Parallelism and Resource Limits

Principle: CPU, I/O, memory and concurrent scans each have their own cap, all controlled by a single `ScanLimits`. The core never uses a global thread pool.

### 5.1 Resources and controls

| Resource | Failure if unbounded | Control |
|---|---|---|
| CPU | Parse workers and sorting saturate all cores and the foreground stutters | Dedicated rayon `ThreadPool` with a capped thread count and lowered thread priority |
| Disk I/O | Large sequential reads fill the disk queue and other I/O latency spikes | Cap on in-flight reads + optional rate limit + low I/O priority |
| Memory | The reader outpaces the workers and buffers pile up | Fixed-size buffer pool (also provides backpressure) |
| Concurrent scans | Several agents trigger scans at once and the load stacks | Service-level semaphore; one scan per volume, duplicate requests share the running task |

### 5.2 Configuration

```rust
pub struct ScanLimits {
    /// Worker threads for parse/sort/aggregate. Default: max(1, cores / 2).
    pub threads: NonZeroUsize,
    /// Read chunk size in bytes. Default: 4 MiB.
    pub chunk_size: usize,
    /// Max buffers alive at once (in flight + queued + being parsed).
    /// Bounds memory to chunk_size * buffers. Default: threads + 2.
    pub buffers: NonZeroUsize,
    /// Max outstanding read requests (overlapped I/O depth). Default: 2.
    pub io_depth: NonZeroUsize,
    /// Optional read throughput cap (token bucket). Default: None.
    pub max_read_bytes_per_sec: Option<NonZeroU64>,
    /// Lower CPU + I/O priority of scan threads. Default: true.
    pub low_priority: bool,
}

pub enum ScanProfile { Background, Balanced, Max }
```

| Profile | threads | io_depth | Rate limit | Low priority |
|---|---|---|---|---|
| Background | 2 | 1 | 200 MB/s | yes |
| Balanced (default) | cores / 2 | 2 | none | yes |
| Max | all cores | 4 | none | no |

Individual fields can override a profile: the CLI takes `--threads 4 --io-depth 1 --max-read 300M`, and MCP `scan_volume` accepts `profile` plus overrides.

### 5.3 CPU: dedicated pool

```rust
let pool = rayon::ThreadPoolBuilder::new()
    .num_threads(limits.threads.get())
    .thread_name(|i| format!("adamite-scan-{i}"))
    .start_handler(move |_| if low_priority { platform::lower_thread_priority() })
    .build()?;

pool.install(|| {
    // Phase 1 parse, Phase 6 par_sort / ext_stats — all parallel work runs here.
    // Nested par_iter inside install() stays on this pool.
});
```

- **The core never calls `rayon::spawn` or relies on the global pool.** All parallel code runs on the pool passed in, so an embedding process (e.g. an MCP server) keeps its own threads.
- **The reader is one extra, mostly I/O-blocked thread** and does not count toward `threads`.
- **Windows:** worker threads use `SetThreadPriority(THREAD_PRIORITY_BELOW_NORMAL)`. A standalone CLI scan may use `PROCESS_MODE_BACKGROUND_BEGIN`, which lowers CPU, I/O and memory-page priority together.

### 5.4 I/O and memory: buffer-pool backpressure

```
            ┌──── free_rx (bounded, N = buffers) ◄──────────────┐
            ▼                                                   │
 reader ── take free buffer ── rate-limit acquire ── read_at ── full_tx ──► worker parse ── return buffer
          (no free buffer → block = backpressure)        (bounded)
```

- **`buffers` aligned buffers are allocated once at startup**, with no further allocation during the scan. Peak memory is `chunk_size × buffers`.
- **The reader must get a free buffer before issuing a read.** When workers fall behind, the reader stalls, so disk throughput matches CPU throughput automatically.
- **`io_depth` caps outstanding reads** (1 for synchronous reads). On HDDs, a depth of 2 saturates sequential bandwidth without starving other programs.
- **Rate limit:** a simple token bucket; `acquire(chunk_len)` before each read, sleeping when tokens run out.
- **Low I/O priority:** `SetFileInformationByHandle(FileIoPriorityHintInfo, IoPriorityHintLow)` on the volume handle.

### 5.5 Service-level scan scheduling

```rust
pub struct ScanScheduler {
    global: Semaphore,                                    // max concurrent scans, default 1
    running: Mutex<HashMap<VolumeId, Shared<ScanTask>>>,  // per-volume dedup
}
```

- **A duplicate request for the same volume** returns the running task's handle; the caller subscribes to its progress.
- **The global semaphore defaults to 1.** Scans of different volumes queue and run serially, so their thread pools never stack beyond the configured limits.
- **Every scan carries a `CancellationToken`.** The reader checks it before each chunk and workers check it per chunk, so a scan stops within tens of milliseconds. MCP exposes this as `cancel_scan`.
- **Queries are mostly single-threaded slice operations.** Only full-scan queries such as `search` run on a small shared pool (2 threads by default).

### 5.6 Impact on the pipeline

- **Phase 1** uses the buffer-pool model above.
- **Phases 4–5** (link, CSR build, aggregation) are single-threaded linear passes and are unaffected.
- **Phase 6** runs `par_sort` and extension stats on the same pool.
- **`ScanStats`** records `threads_used`, `peak_buffer_bytes` and `throttled_ms`, so the limits can be verified.

---

## 6. Implementation Order

1. **`adamite-ntfs`:** boot sector, fixup, attribute and runlist parsing with synthetic builders, golden vectors and fuzzing. Generate compressed NTFS images and independent JSON manifests using `deno task fixtures` on Linux with ntfs-3g, zstd, attr and sudo; tests read those images on Linux and Windows without mounting them.
2. **`adamite-core` single-threaded:** Phases 0–6 end to end, cross-checked against `dir /s` and WizTree.
3. **Parallelism and I/O:** dedicated pool, buffer-pool backpressure, aligned reads, `$BITMAP` skipping, resource limits.
4. **Snapshot persistence + CLI.**
5. **MCP (rmcp).**
6. **USN incremental refresh + daemon.**

## 7. Conventions

Rust crates use edition 2024 and toolchain 1.99. Keep `rust-toolchain.toml` and the workspace `rust-version` synchronized.

Production types must not carry fields, variants or hook state used only by tests; tests observe real outputs or existing production state.

Every crate uses snafu errors. The parser configures snafu with `default-features = false` and `features = ["rust_1_81"]` for `no_std` support.

Crates that log use tracing, beginning with `adamite-core`; binaries install tracing-subscriber. `adamite-ntfs` returns parsed values and errors without logging.

Task automation and fixture generation use Deno TypeScript scripts, invoked through the root `deno.json` with `deno task`.

Do not add bash, Python or just scripts; CI steps invoke only `cargo` or `deno` one-liners.

Implementation plans live in `docs/plans/`, one file per piece of work.
