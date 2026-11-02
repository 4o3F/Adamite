#![no_main]
mod support;
use libfuzzer_sys::fuzz_target;
fuzz_target!(|data: &[u8]| {
    if let Ok(value) = adamite_ntfs::parse_boot_sector(data) {
        support::geometry(value);
        let count = data
            .get(..8)
            .and_then(|bytes| bytes.try_into().ok())
            .map(u64::from_le_bytes)
            .unwrap_or(0);
        let _ = value.cluster_byte_range(count, count);
    }
});
