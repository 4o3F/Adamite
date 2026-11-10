# Full-scan fuzzing

This standalone nightly package drives `Scanner` through its public request/feed API. Synthetic images contain 24 records in 64 KiB, resident and non-resident DATA, ADS, DOS aliases and hard links. Fragmented layouts require two extension discoveries and assemble records across extent boundaries. Raw inputs and up to 16 XOR mutations exercise malformed storage.

Inputs have a 12-byte header: mode (`% 3`: contiguous, fragmented, raw), transport (low two bits select cap 1–4, next two select chunks 512/1024/2048/4096), layout options, eight cyclic event bytes, and mutation count (`% 17`). Each mutation is a little-endian u32 offset plus one XOR byte; the remaining bytes supply the raw image. Images are truncated to 1 MiB. Event low bits choose a response permutation; bits 5–7 select unknown-id, wrong-length and duplicate feeds.

Preflight rejects advertised MFT data above 4096 records and attribute-list values above 256 KiB. It decodes record 0 and conservatively checks possible eight-byte-aligned list headers throughout the image, including extension storage. Possible list headers whose fixed fields cross a 512-byte boundary are skipped because fixups or fragmented storage prevent a reliable size check. These bounds belong to the harness.

Every request must be nonempty, at most one chunk and free of offset overflow. Outstanding requests, including accepted responses awaiting consumption, must fit `cap × chunk_size`. Requests outside the supplied image stop that input immediately. Other inputs must reach Done or an error within `4096 + 64 × records + 16 × image_sectors` calls, with partial sectors rounded up. A second scanner receives only valid feeds; subsequent steps and final outputs must agree after injected invalid feeds. Completed snapshots undergo linear CSR and aggregation checks.

From this directory:

```sh
cargo +nightly fuzz build scan
cargo +nightly fuzz run scan -- -max_total_time=30 -max_len=262144 -timeout=2 -rss_limit_mb=2048
cargo test --test harness --locked
cargo clippy --all-targets --locked -- -D warnings
```

Generate or restore the deterministic corpus from the repository root:

```sh
deno run --frozen --allow-run=zstd --allow-read=tests/fixtures/ntfs --allow-write=crates/adamite-core/fuzz/corpus scripts/fuzz_corpus.ts --core
```

The corpus contains 128 synthetic configurations and three reduced fixture images, totaling 48,676 bytes under the 64 KiB budget. Fixture reductions retain real boot sectors and records 0/5, use one-sector clusters, relocate the MFT into six records, rewrite its DATA mapping, remove its attribute list, remap the root's non-resident streams and restore update-sequence protection. The harness tests require every deterministic seed to reach Done. Regenerate after fuzzing to remove coverage-generated inputs before committing.
