#![no_main]
mod support;
use libfuzzer_sys::fuzz_target;
fuzz_target!(|data: &[u8]| {
    let mut attributes = adamite_ntfs::AttributeIter::from_region(data);
    for attribute in attributes.by_ref() {
        if let Ok(value) = attribute {
            support::attribute(value);
        }
    }
    std::hint::black_box(attributes.next());
});
