#![allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]
//! Counts allocations while classifying records and exhausting attributes and runs.

use adamite_fixtures::builders::{
    AttrBuilder, AttrListBuilder, AttrListEntryModel, FileNameBuilder, NonResidentModel,
    RecordBuilder,
};
use adamite_ntfs::{
    AttributeList, Bitmap, Data, FileName, IndexAllocation, RecordClass, ReparsePoint,
    StandardInformation, classify,
};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
};

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

struct CountingAllocator;

fn record_allocation() {
    if COUNTING.try_with(Cell::get).unwrap_or(false) {
        let _ = ALLOCATIONS.try_with(|count| count.set(count.get().saturating_add(1)));
    }
}

// SAFETY: All allocation operations delegate to System with the caller's layout and pointer.
#[allow(unsafe_code)]
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record_allocation();
        // SAFETY: The caller supplies the layout required by GlobalAlloc.
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record_allocation();
        // SAFETY: The caller supplies the layout required by GlobalAlloc.
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        record_allocation();
        // SAFETY: Pointer, layout and new size retain the caller's allocator contract.
        unsafe { System.realloc(pointer, layout, new_size) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: Pointer and layout retain the caller's allocator contract.
        unsafe { System.dealloc(pointer, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

struct CountingGuard;
impl Drop for CountingGuard {
    fn drop(&mut self) {
        COUNTING.with(|enabled| enabled.set(false));
    }
}

#[test]
fn record_attribute_and_runlist_hot_path_does_not_allocate() {
    let mut named = AttrBuilder::resident(0x80, vec![0x61; 600]);
    named.name = vec![0xd800, 0, 0x61];
    let attributes = vec![
        named.build(),
        AttrBuilder::non_resident(
            0x80,
            NonResidentModel {
                highest: 2,
                mapping_pairs: vec![0x11, 1, 7, 0x01, 1, 0x11, 1, 2, 0],
                ..Default::default()
            },
        )
        .build(),
        AttrBuilder::resident(0x10, vec![0; 72]).build(),
        AttrBuilder::resident(0x30, FileNameBuilder::default().build()).build(),
        AttrBuilder::resident(0xc0, vec![0; 8]).build(),
        AttrBuilder::resident(0xb0, vec![5]).build(),
        AttrBuilder::resident(
            0x20,
            AttrListBuilder {
                entries: vec![AttrListEntryModel::default()],
                zero_tail: 0,
            }
            .build(),
        )
        .build(),
        AttrBuilder::non_resident(0xa0, NonResidentModel::default()).build(),
    ];
    let mut records: Vec<_> = [0, 1u64 << 48]
        .into_iter()
        .map(|base| {
            RecordBuilder {
                size: 4096,
                base,
                attributes: attributes.clone(),
                ..Default::default()
            }
            .build()
        })
        .collect();
    ALLOCATIONS.with(|count| count.set(0));
    COUNTING.with(|enabled| enabled.set(true));
    let guard = CountingGuard;
    let mut attributes_seen = 0;
    let mut runs_seen = 0;
    for bytes in &mut records {
        let record = match classify(bytes) {
            RecordClass::Base(record) | RecordClass::Extension(record) => record,
            other => panic!("unexpected class {other:?}"),
        };
        for attribute in record.attributes() {
            let attribute = attribute.unwrap();
            attributes_seen += 1;
            for unit in attribute.name().units() {
                std::hint::black_box(unit);
            }
            for character in attribute.name().decode_strict() {
                std::hint::black_box(character).ok();
            }
            for character in attribute.name().decode_lossy() {
                std::hint::black_box(character);
            }
            match attribute.attribute_type().raw() {
                0x10 => {
                    std::hint::black_box(StandardInformation::from_attribute(attribute).unwrap());
                }
                0x20 => {
                    for entry in AttributeList::from_attribute(attribute).unwrap() {
                        std::hint::black_box(entry.unwrap());
                    }
                }
                0x30 => {
                    std::hint::black_box(FileName::from_attribute(attribute).unwrap());
                }
                0x80 => {
                    std::hint::black_box(Data::from_attribute(attribute).unwrap());
                }
                0xa0 => {
                    std::hint::black_box(IndexAllocation::from_attribute(attribute).unwrap());
                }
                0xb0 => {
                    let bitmap = Bitmap::new(attribute.resident_value().unwrap(), 3).unwrap();
                    for bit in bitmap.iter() {
                        std::hint::black_box(bit);
                    }
                    for index in bitmap.iter_set() {
                        std::hint::black_box(index);
                    }
                }
                0xc0 => {
                    std::hint::black_box(ReparsePoint::from_attribute(attribute).unwrap());
                }
                kind => panic!("unexpected attribute type {kind:#x}"),
            }
            if let Some(descriptor) = attribute.non_resident() {
                for run in descriptor.runs() {
                    std::hint::black_box(run.unwrap());
                    runs_seen += 1;
                }
            }
        }
    }
    drop(guard);
    assert_eq!((attributes_seen, runs_seen), (16, 8));
    assert_eq!(ALLOCATIONS.with(Cell::get), 0);
}

#[test]
fn real_fixture_records_do_not_allocate_during_classification_or_iteration() {
    let mut records = Vec::new();
    for name in ["basic-4k", "frag-512", "4kn-64k"] {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("../../tests/fixtures/ntfs/{name}.json"));
        let fixture = adamite_fixtures::Fixture::load(&path).unwrap();
        let reference = adamite_fixtures::ReferenceMft::from_fixture(&fixture).unwrap();
        records.extend(
            fixture
                .manifest()
                .entries
                .iter()
                .map(|entry| reference.record(entry.record).unwrap().to_vec()),
        );
    }
    ALLOCATIONS.with(|count| count.set(0));
    COUNTING.with(|enabled| enabled.set(true));
    let guard = CountingGuard;
    let mut seen = 0;
    for bytes in &mut records {
        let record = match classify(bytes) {
            RecordClass::Base(record) => record,
            other => panic!("unexpected fixture class {other:?}"),
        };
        for attribute in record.attributes() {
            let attribute = attribute.unwrap();
            for unit in attribute.name().units() {
                std::hint::black_box(unit);
            }
            for character in attribute.name().decode_strict() {
                std::hint::black_box(character).ok();
            }
            for character in attribute.name().decode_lossy() {
                std::hint::black_box(character);
            }
            if let Some(descriptor) = attribute.non_resident() {
                for run in descriptor.runs() {
                    std::hint::black_box(run.unwrap());
                }
            }
            seen += 1;
        }
    }
    drop(guard);
    assert!(seen > 200);
    assert_eq!(ALLOCATIONS.with(Cell::get), 0);
}
