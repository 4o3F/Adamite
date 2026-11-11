# adamite-profile

Unpublished profiling support shared by Adamite executables. Libraries that perform scans, I/O and terminal rendering emit `tracing` spans without depending on this crate.

## Use

```rust
use adamite_profile::{ChromeProfile, CountingAllocator};

#[global_allocator]
static ALLOC: CountingAllocator = CountingAllocator::new();

fn main() -> Result<(), adamite_profile::ProfileError> {
    let _profile = ChromeProfile::install("trace.json", &ALLOC)?;
    tracing::info_span!("example").in_scope(|| {});
    Ok(())
}
```

`ChromeProfile::install` creates the destination, installs one global subscriber and returns a guard. Keep the guard alive until command work and span handles finish; dropping it joins the trace writer and completes the JSON file. File creation and subscriber installation failures return `ProfileError`. Open the trace in Perfetto or `chrome://tracing`.

## Heap measurements

`CountingAllocator` wraps `std::alloc::System` and uses relaxed atomic counters for current and peak requested bytes. Failed allocations leave the counters unchanged. Successful reallocations adjust the live count by the size difference. `peak()` reads the current interval peak. `take_peak()` returns that peak and resets the next interval to the current live count, then folds concurrent allocation growth into the next interval. While profiling is active, the heap layer is the sole consumer of `take_peak()` so every interval is included.

`HeapLayer` keeps each open span's starting live count and running maximum in a mutex-protected map. At every span open/close boundary, it takes the allocator peak since the previous boundary and folds it into every open span before registering or removing that span. This tracks each span's peak across nested and overlapping lifetimes, regardless of close order. Events are emitted after releasing the mutex.

On close the layer emits a `heap_usage` event with `span_name`, `span_id`, `heap_current`, `heap_peak`, `heap_delta` and `process_peak`. `heap_peak` is the maximum live requested bytes while that span was open; `process_peak` is the maximum observed by the layer across all intervals, including the allocator's history before the first span. The delta is signed, so freeing data can produce a negative value. Measurements cover all allocations through the supplied allocator, including other threads and tracing itself. Counts exclude allocator metadata.

The registry places `HeapLayer` inside the Chrome layer so its close callback emits the heap event before Chrome closes the span. `TraceStyle::Async` records each span's creation-to-close lifetime, allowing a scan stage to retain a handle across driver calls while entering it only during each call. `include_args(true)` includes heap event fields and stage/batch counts in the JSON `args`. In tracing-chrome 0.7.2, its `JsonVisitor` serializes field values as strings through `record_debug`; numeric heap values therefore appear as decimal strings. The heap event identifies its span explicitly because Chrome instant events do not preserve tracing parent IDs.

## Unsafe boundary and validation

Only `src/allocator.rs` overrides the workspace unsafe-code denial. It forwards the caller's allocation contracts to `System`, with one unsafe operation per documented block and Clippy checks for undocumented unsafe blocks. Counter updates never allocate.

```sh
cargo test -p adamite-profile
cargo clippy -p adamite-profile --all-targets -- -D warnings
cargo clippy -p adamite-profile --all-targets --target x86_64-pc-windows-gnu -- -D warnings
```

Unit tests allocate directly through the allocator type to verify live counts, growth, shrinkage, zeroed allocation and interval-peak consumption. Nested and non-LIFO overlapping spans use known allocation sizes to check exact span peaks, signed deltas and the separate process maximum. A separate test installs the guard and parses the finished trace, checking a named span and heap arguments.
