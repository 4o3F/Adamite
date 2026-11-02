#![allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

//! Parser errors remain small, owned values implementing the core error trait.

use core::{any, error::Error, mem};

use adamite_ntfs::fixup::FixupError;
use adamite_ntfs::{
    AttributeError, AttributeListError, BitmapError, FileRefError, GeometryError, RecordError,
    RunlistError, Utf16Error, ValueError,
};

fn assert_parser_error<E: Error + Copy + Send + Sync + 'static>() {
    assert!(
        mem::size_of::<E>() <= 24,
        "{} occupies {} bytes; parser errors must fit in 24 bytes",
        any::type_name::<E>(),
        mem::size_of::<E>()
    );
}

#[test]
fn every_public_error_is_copy_and_at_most_24_bytes() {
    assert_parser_error::<FileRefError>();
    assert_parser_error::<AttributeError>();
    assert_parser_error::<ValueError>();
    assert_parser_error::<BitmapError>();
    assert_parser_error::<AttributeListError>();
    assert_parser_error::<GeometryError>();
    assert_parser_error::<RecordError>();
    assert_parser_error::<FixupError>();
    assert_parser_error::<RunlistError>();
    assert_parser_error::<Utf16Error>();
}

#[test]
fn attribute_list_value_errors_preserve_the_copy_source() {
    let value = ValueError::WrongAttributeType {
        expected: 0x20,
        actual: 0x80,
    };
    let error = AttributeListError::Value { source: value };
    assert_eq!(
        error.source().unwrap().downcast_ref::<ValueError>(),
        Some(&value)
    );
    assert!(error.to_string().contains("0x80"));
    assert!(error.to_string().contains("0x20"));
}

#[test]
fn leaf_errors_have_display_messages_and_no_sources() {
    let file_reference = FileRefError::RecordOutOfRange { record: 1 << 48 };
    assert!(file_reference.to_string().contains("281474976710656"));
    assert!(file_reference.source().is_none());

    let odd_length = Utf16Error::OddLength { len: 3 };
    assert!(odd_length.to_string().contains('3'));
    assert!(odd_length.source().is_none());

    let surrogate = Utf16Error::UnpairedSurrogate {
        index: 7,
        unit: 0xd800,
    };
    assert!(surrogate.to_string().contains("0xd800"));
    assert!(surrogate.to_string().contains('7'));
    assert!(surrogate.source().is_none());

    #[cfg(feature = "alloc")]
    {
        assert!(!Utf16Error::AllocationFailed.to_string().is_empty());
        assert!(Utf16Error::AllocationFailed.source().is_none());
    }
}
