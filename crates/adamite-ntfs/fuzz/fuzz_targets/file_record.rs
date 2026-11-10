#![no_main]
mod support;
use adamite_ntfs::record::{RecordClass, RecordHeader};
use libfuzzer_sys::fuzz_target;
fuzz_target!(|data: &[u8]| {
    if let Ok(value) = RecordHeader::peek(data) {
        support::header(value);
    }
    let mut bytes = data.to_vec();
    match RecordClass::classify(&mut bytes) {
        RecordClass::Base(record) | RecordClass::Extension(record) => {
            support::header(record.header());
            std::hint::black_box(record.as_bytes());
            for value in record.attributes().flatten() {
                support::attribute(value);
            }
        }
        RecordClass::NotInUse(value) => support::header(value),
        _ => {}
    }
});
