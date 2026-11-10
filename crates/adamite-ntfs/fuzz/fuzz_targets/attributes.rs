#![no_main]
mod support;
use libfuzzer_sys::fuzz_target;
fuzz_target!(|data: &[u8]| {
    let mut attributes = adamite_ntfs::AttributeIter::from_region(data);
    for value in attributes.by_ref().flatten() {
        support::attribute(value);
    }
    std::hint::black_box(attributes.next());
});
