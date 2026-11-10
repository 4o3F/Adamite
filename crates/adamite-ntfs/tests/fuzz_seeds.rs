#![allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]
//! Generated typed-value seeds must select and satisfy their intended decoder.
#[path = "../fuzz/fuzz_targets/typed_values/selectors.rs"]
mod selectors;

use adamite_fixtures::builders::RecordBuilder;
use adamite_ntfs::{
    record::FileRecord,
    utf16::Utf16Le,
    value::{
        attribute_list::AttributeList, bitmap::Bitmap, data::Data, file_name::FileName,
        index_allocation::IndexAllocation, reparse_point::ReparsePoint,
        standard_information::StandardInformation,
    },
};
use std::{collections::BTreeSet, fs, path::Path};

fn check_attributes(region: &[u8], selector: u8) {
    let mut offset = 0;
    let mut attributes = Vec::new();
    loop {
        let kind = u32::from_le_bytes(region[offset..offset + 4].try_into().unwrap());
        if kind == u32::MAX {
            break;
        }
        let len = u32::from_le_bytes(region[offset + 4..offset + 8].try_into().unwrap()) as usize;
        assert!(len >= 24);
        attributes.push(region[offset..offset + len].to_vec());
        offset += len;
    }
    assert!(!attributes.is_empty());
    assert_eq!(offset + 4, region.len());
    let mut raw = RecordBuilder {
        size: 65536,
        attributes,
        ..Default::default()
    }
    .build();
    let record = FileRecord::parse(&mut raw).unwrap();
    for attribute in record.attributes() {
        let attribute = attribute.unwrap();
        let descriptor = if selector == selectors::DATA {
            Data::from_attribute(attribute).unwrap().non_resident()
        } else {
            Some(
                IndexAllocation::from_attribute(attribute)
                    .unwrap()
                    .descriptor(),
            )
        };
        if let Some(descriptor) = descriptor {
            descriptor.runs().collect::<Result<Vec<_>, _>>().unwrap();
        }
    }
}

#[test]
fn every_typed_seed_decodes_with_its_named_selector() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("fuzz/corpus/typed_values");
    let mut seen = BTreeSet::new();
    for file in fs::read_dir(root).unwrap() {
        let file = file.unwrap();
        if !file.file_type().unwrap().is_file() {
            continue;
        }
        // Extractor seeds have SHA-256 names; fuzzing also adds malformed inputs.
        let name = file.file_name();
        if !name.to_str().is_some_and(|name| {
            name.len() == 64 && name.bytes().all(|byte| byte.is_ascii_hexdigit())
        }) {
            continue;
        }
        let input = fs::read(file.path()).unwrap();
        let (&selector, bytes) = input.split_first().unwrap();
        assert!(
            selector < selectors::COUNT,
            "{} has a noncanonical selector",
            file.path().display()
        );
        seen.insert(selector);
        match selector {
            selectors::STANDARD_INFORMATION => {
                StandardInformation::parse(bytes).unwrap();
            }
            selectors::FILE_NAME => {
                FileName::parse(bytes).unwrap();
            }
            selectors::REPARSE_POINT => {
                ReparsePoint::parse(bytes).unwrap();
            }
            selectors::ATTRIBUTE_LIST => {
                AttributeList::new(bytes)
                    .unwrap()
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap();
            }
            selectors::BITMAP => {
                let count =
                    usize::try_from(u64::from_le_bytes(bytes[..8].try_into().unwrap())).unwrap();
                let bitmap = Bitmap::new(&bytes[8..], count).unwrap();
                assert_eq!(bitmap.iter().count(), count);
            }
            selectors::UTF16 => {
                let name = Utf16Le::new(bytes).unwrap();
                assert_eq!(name.units().count(), name.len_units());
                name.decode_strict().for_each(|result| {
                    std::hint::black_box(result).ok();
                });
                name.decode_lossy().for_each(|character| {
                    std::hint::black_box(character);
                });
            }
            selectors::DATA | selectors::INDEX_ALLOCATION => check_attributes(bytes, selector),
            _ => panic!("unexpected selector {selector}"),
        }
    }
    assert_eq!(
        seen,
        BTreeSet::from([
            selectors::STANDARD_INFORMATION,
            selectors::FILE_NAME,
            selectors::REPARSE_POINT,
            selectors::ATTRIBUTE_LIST,
            selectors::BITMAP,
            selectors::UTF16,
            selectors::DATA,
            selectors::INDEX_ALLOCATION,
        ])
    );
}
