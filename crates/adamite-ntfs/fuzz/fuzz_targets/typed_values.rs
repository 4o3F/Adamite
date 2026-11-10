#![no_main]
#[path = "typed_values/selectors.rs"]
mod selectors;
mod support;
use adamite_ntfs::{
    attribute::iter::AttributeIter,
    utf16::Utf16Le,
    value::{
        attribute_list::AttributeList, bitmap::Bitmap, data::Data, file_name::FileName,
        index_allocation::IndexAllocation, reparse_point::ReparsePoint,
        standard_information::StandardInformation,
    },
};
use libfuzzer_sys::fuzz_target;
fuzz_target!(|data: &[u8]| {
    let Some((&selector, bytes)) = data.split_first() else {
        return;
    };
    let selector = selector % selectors::COUNT;
    match selector {
        selectors::STANDARD_INFORMATION => {
            if let Ok(value) = StandardInformation::parse(bytes) {
                support::standard(value);
            }
        }
        selectors::FILE_NAME => {
            if let Ok(value) = FileName::parse(bytes) {
                support::file_name(value);
            }
        }
        selectors::REPARSE_POINT => {
            if let Ok(value) = ReparsePoint::parse(bytes) {
                std::hint::black_box((value.tag(), value.data_length(), value.payload()));
            }
        }
        selectors::ATTRIBUTE_LIST => {
            if let Ok(value) = AttributeList::new(bytes) {
                support::list_entries(value);
            }
        }
        selectors::BITMAP => {
            let logical = bytes
                .get(..8)
                .and_then(|value| value.try_into().ok())
                .map(u64::from_le_bytes)
                .and_then(|value| usize::try_from(value).ok())
                .unwrap_or(0);
            if let Ok(value) = Bitmap::new(bytes.get(8..).unwrap_or_default(), logical) {
                support::bitmap(value);
            }
        }
        selectors::UTF16 => {
            if let Ok(value) = Utf16Le::new(bytes) {
                support::name(value);
            }
        }
        selectors::DATA | selectors::INDEX_ALLOCATION => {
            for value in AttributeIter::from_region(bytes).flatten() {
                if selector == selectors::DATA {
                    if let Ok(data) = Data::from_attribute(value) {
                        support::name(data.name());
                        std::hint::black_box((
                            data.flags().raw(),
                            data.resident_value(),
                            data.logical_size(),
                        ));
                        if let Some(descriptor) = data.non_resident() {
                            support::descriptor(descriptor);
                        }
                    }
                } else if let Ok(index) = IndexAllocation::from_attribute(value) {
                    support::name(index.name());
                    std::hint::black_box(index.flags().raw());
                    support::descriptor(index.descriptor());
                }
            }
        }
        _ => {}
    }
});
