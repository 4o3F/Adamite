# Plan: count mapped clusters as allocation for ordinary streams

## Why

- `adamite-core` reports a non-resident stream's allocation from its attribute header: `compressed_size` when the attribute is flagged sparse or compressed, `allocated_size` otherwise.
- An unflagged stream may still contain sparse (LCN-less) runs. NTFS creates `$BadClus:$Bad` as a volume-sized unflagged stream whose runlist is a single hole; only bad clusters are ever mapped. Its header `allocated_size` equals the volume size while no cluster is occupied.
- On every fixture this inflates the root allocation beyond the volume: `basic-4k` reports 18.9 MiB allocated on a 16 MiB volume (`$BadClus` 16.0 MiB), `frag-512` and `4kn-64k` likewise. The CLI `top`/`ls`/`info` and the TUI show the wrong figures. Reduced MFT-only images show the same header/run disagreement for `$MFT:$BITMAP`.

## What Changes

- Ordinary (unflagged) non-resident streams report allocation as the clusters actually mapped by their runlists, summed over all segments, times the cluster size.
- Flagged sparse/compressed streams keep first-segment `compressed_size`; resident streams stay zero.
- Test oracles derive ordinary-stream allocation from an independent mapped-run reference in `adamite-fixtures` instead of manifest headers.

## Scope

In scope: core decode/model/merge, `adamite-fixtures` reference helper, core/CLI/TUI test expectations, the persist fuzz seeds affected by the new values, documentation of the allocation rule.

Out of scope: regenerating fixture images or manifests (header measurements stay as recorded), changing the `.adm` format or version, changing the sparse/compressed rule, bootstrap `$MFT` mapping (already rejects holes), the `scripts/fixtures` filler compaction formula (committed fillers are unaffected; noted for future fillers with unflagged holes).

## Design decisions

1. D1. Rule. For an unflagged non-resident stream, `alloc = Σ(length of runs with an LCN, over every accepted segment) × cluster_size`, used directly — not `min(allocated_size, …)`: mapping pairs identify occupied clusters, and preallocation beyond the logical size is real allocation. LCN 0 counts. Sparse/compressed flagged streams keep `sizes.compressed()` (their padding runs carry no LCN, so the rule is consistent, and the existing contract is unchanged). Resident streams: 0.
2. D2. Data flow. `StreamStorage::NonResident` gains `mapped_clusters: u64`, accumulated with checked addition in the existing run loop of `mft/ingest/decode.rs` (alongside `validate_geometry` and `run_count`). `StreamTotals::from_segments` keeps all current validation, sums `mapped_clusters` across segments with checked addition and multiplies by `cluster_size` with checked multiplication; overflow is the existing fatal `ScanError::Overflow`. Applies uniformly to DATA, ADS, INDEX_ALLOCATION, BITMAP, ATTRIBUTE_LIST and other non-resident attributes.
3. D3. Independent oracle. `adamite-fixtures` gains a reference helper over `ReferenceMft::records()` that groups non-resident attributes by base record, attribute type and raw name (extensions mapped to their base) and counts LCN-bearing run lengths across segments using `adamite-ntfs` runlist decoding. It does not reuse `add_runs()` (which rejects holes for MFT reconstruction). This mirrors the existing independent-reconstruction pattern: it is the oracle, not a copy of core logic, and shares only the NTFS parser. Manifests keep their header measurements, including `$Bad`'s volume-sized `allocated_size`.
4. D4. Expected results. Root allocation after the fix: `basic-4k` 3,039,232, `frag-512` 31,450,112, `4kn-64k` 3,538,944 bytes; `$BadClus` allocation 0 on all three. Root `total_alloc ≤ geometry.total_bytes()` on every fixture.
5. D5. Tests.
   - Fixtures helper: `$Bad` is 0 on all fixtures; ordinary and extension-backed streams match header allocation where no holes exist.
   - Merge unit tests: mixed mapped/hole and hole-only unflagged streams; header smaller and larger than mapped bytes; empty stream; multi-extent stream with holes and the extension listed before the base; compressed stream with padding runs unchanged; `merge_allocation_overflow_is_fatal` reworked to overflow mapped-byte multiplication and entry totals.
   - Core fixture oracles (`tests/fixtures.rs`, `scan/scanner/tests/{merge,done}.rs`) use the D3 reference; `$BadClus.alloc() == 0`; D4 bound.
   - CLI exact text and TUI expectations updated to the corrected totals and rankings.
   - Tiny `.adm` golden stays byte-identical.
6. D6. Persist corpus. Regenerate with `deno task fuzz-corpus --core`, twice, requiring identical output. Expected: the 8 seeds derived from the reduced `basic-4k` and `4kn-64k` images are replaced (new content hashes); every other persist seed, the scan corpus, NTFS corpora, images, manifests and the golden stay byte-identical.
7. D7. Documentation. ARCHITECTURE §2.3 (decode keeps mapped-cluster counts), §3.1 and the allocation rule paragraph, core README and `lib.rs` crate docs state D1.

## Tasks

Each milestone ends with one reviewed commit and `deno task check` green; each Codex run stays under 40 minutes.

### S1 — Independent mapped-allocation reference

- [x] Add the D3 helper to `adamite-fixtures` with its D5 tests; no core change.

### S2 — Count mapped clusters in core

- [ ] Implement D1/D2; migrate the core oracles to the S1 reference; add the D5 merge and fixture tests; update CLI and TUI expectations (D4/D5) in the same commit so the gate stays green.

### S3 — Persist seeds and documentation

- [ ] Regenerate the persist corpus per D6 and verify the exact replacement set; update documentation per D7; `deno task check --fuzz` green.

## Acceptance criteria

- [ ] On all three fixtures, `$BadClus` allocation is 0, root allocation equals D4, and root `total_alloc ≤` volume bytes.
- [ ] Ordinary, multi-extent, sparse, compressed and hole cases have targeted tests; allocation overflow stays fatal.
- [ ] Oracles derive allocation from the independent reference, not from core.
- [ ] `deno task check` and `deno task check --fuzz` pass; only the D6 persist seeds change among committed binary artifacts; documentation states D1.
