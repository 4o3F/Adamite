//! Requested-heap regression budgets measured in an isolated production benchmark process.

use serde_json::Value;
use std::{error::Error, process::Command};

// M3 reference mix: 40,000 in-use records, 50,032 MFT slots, 40,001 nodes.
// CountingAllocator measured 21,376,945 / 50,032 = 427.27 scan bytes per slot
// and 5,330,016 / 40,001 = 133.25 snapshot bytes per node in a debug process.
// Multiply each by 1.10 and round up. Linux and Windows CI both use 64-bit pointers;
// these budgets count requested allocation bytes, including profiling overhead, not RSS.
const MAX_SCAN_HEAP_BYTES_PER_SLOT: u64 = 470;
const MAX_SNAPSHOT_HEAP_BYTES_PER_NODE: u64 = 147;

#[test]
fn reference_scan_stays_within_heap_budgets() -> Result<(), Box<dyn Error>> {
    let directory = tempfile::tempdir()?;
    // libtest threads share the global allocator. The child owns its CountingAllocator
    // and generates the image before starting a fresh measurement process.
    let output = Command::new(env!("CARGO_BIN_EXE_adamite-bench"))
        .args(["--in-use", "40000", "--json", "--output-dir"])
        .arg(directory.path())
        .output()?;
    assert!(
        output.status.success(),
        "benchmark failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout)?;
    let slots = result
        .get("slots")
        .and_then(Value::as_u64)
        .ok_or("missing slots")?;
    let nodes = result
        .get("nodes")
        .and_then(Value::as_u64)
        .ok_or("missing nodes")?;
    let peak = result
        .get("scan_peak_heap_bytes")
        .and_then(Value::as_u64)
        .ok_or("missing scan peak")?;
    let snapshot = result
        .get("snapshot_heap_bytes")
        .and_then(Value::as_u64)
        .ok_or("missing snapshot heap")?;
    assert_eq!(slots, 50_032, "reference slot count changed");
    assert_eq!(nodes, 40_001, "reference node count changed");
    let scan_budget = slots
        .checked_mul(MAX_SCAN_HEAP_BYTES_PER_SLOT)
        .ok_or("scan budget overflow")?;
    let snapshot_budget = nodes
        .checked_mul(MAX_SNAPSHOT_HEAP_BYTES_PER_NODE)
        .ok_or("snapshot budget overflow")?;
    assert!(
        peak <= scan_budget,
        "scan requested {peak} bytes for {slots} slots; budget {scan_budget}"
    );
    assert!(
        snapshot <= snapshot_budget,
        "snapshot requested {snapshot} bytes for {nodes} nodes; budget {snapshot_budget}"
    );
    Ok(())
}
