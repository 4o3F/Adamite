#![no_main]
mod support;
use libfuzzer_sys::fuzz_target;
fuzz_target!(|data: &[u8]| {
    if let Some(range) = support::vcn(data) {
        let bytes = data.get(16..).unwrap_or_default();
        support::runs(adamite_ntfs::Runlist::new(bytes, range));
        for run in adamite_ntfs::Runlist::new(bytes, range) {
            std::hint::black_box(run).ok();
        }
    }
});
