//! Complete ATTRIBUTE_LIST value decoding for MFT bootstrap segment references.

use super::{Attribute, AttributeType, ValueError, resident_value};
use crate::{FileRef, Utf16Le, bytes};
use core::iter::FusedIterator;
use snafu::{OptionExt, ResultExt, Snafu, ensure};

/// One attribute-segment reference; resolving and joining segments belongs to the core.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttributeListEntry<'a> {
    type_code: AttributeType,
    lowest_vcn: u64,
    segment: FileRef,
    id: u16,
    name: Utf16Le<'a>,
}

impl<'a> AttributeListEntry<'a> {
    /// Returns the original type code at entry offset 0x00.
    pub const fn attribute_type(self) -> AttributeType {
        self.type_code
    }
    /// Returns the nonnegative lowest VCN at entry offset 0x08.
    pub const fn lowest_vcn(self) -> u64 {
        self.lowest_vcn
    }
    /// Returns the complete segment reference at entry offset 0x10.
    pub const fn segment_reference(self) -> FileRef {
        self.segment
    }
    /// Returns the segment's attribute instance identifier at entry offset 0x18.
    pub const fn attribute_id(self) -> u16 {
        self.id
    }
    /// Borrows the original name units, empty for an unnamed entry.
    pub const fn name(self) -> Utf16Le<'a> {
        self.name
    }
}

/// Fused iterator over one complete attribute-list value of at most 256 KiB.
#[derive(Debug, Clone)]
pub struct AttributeList<'a> {
    input: &'a [u8],
    cursor: usize,
    finished: bool,
}

impl<'a> AttributeList<'a> {
    /// Borrows a complete list value supplied by the caller.
    ///
    /// # Errors
    ///
    /// Returns [`AttributeListError::ValueTooLarge`] above 262144 bytes.
    /// Entry errors are yielded lazily by the iterator.
    pub fn new(input: &'a [u8]) -> Result<Self, AttributeListError> {
        ensure!(
            input.len() <= 262144,
            ValueTooLargeSnafu { len: input.len() }
        );
        Ok(Self {
            input,
            cursor: 0,
            finished: false,
        })
    }
    /// Checks ATTRIBUTE_LIST type and borrows its complete resident value.
    ///
    /// # Errors
    ///
    /// Returns [`AttributeListError::Value`] wrapping [`ValueError`] for a type
    /// or storage mismatch, or [`AttributeListError::ValueTooLarge`] for an
    /// oversized value. Use [`Self::new`] for complete values read from non-resident storage.
    pub fn from_attribute(attribute: Attribute<'a>) -> Result<Self, AttributeListError> {
        Self::new(resident_value(attribute, AttributeType::ATTRIBUTE_LIST).context(ValueSnafu)?)
    }
    fn read_next(&mut self) -> Result<Option<AttributeListEntry<'a>>, AttributeListError> {
        let input = self.input.get(self.cursor..).context(TruncatedEntrySnafu)?;
        if input.len() < 26 {
            ensure!(input.iter().all(|&byte| byte == 0), TruncatedEntrySnafu);
            return Ok(None);
        }
        let length = bytes::read_u16(input, 4).context(TruncatedEntrySnafu)?;
        ensure!(length >= 26, InvalidEntryLengthSnafu { length });
        let entry = input
            .get(..usize::from(length))
            .context(EntryPastValueSnafu)?;
        let count = bytes::read_u8(entry, 6).context(TruncatedEntrySnafu)?;
        let name = if count == 0 {
            &[][..]
        } else {
            let offset = usize::from(bytes::read_u8(entry, 7).context(TruncatedEntrySnafu)?);
            ensure!(offset >= 26, InvalidNameRangeSnafu);
            let range = bytes::scaled_range(offset, usize::from(count), 2)
                .context(InvalidNameRangeSnafu)?;
            entry.get(range).context(InvalidNameRangeSnafu)?
        };
        let lowest = bytes::read_i64(entry, 8).context(TruncatedEntrySnafu)?;
        let lowest_vcn = u64::try_from(lowest)
            .ok()
            .context(NegativeLowestVcnSnafu { value: lowest })?;
        let result = AttributeListEntry {
            type_code: AttributeType::from_raw(
                bytes::read_u32(entry, 0).context(TruncatedEntrySnafu)?,
            ),
            lowest_vcn,
            segment: FileRef::from_raw(bytes::read_u64(entry, 16).context(TruncatedEntrySnafu)?),
            id: bytes::read_u16(entry, 24).context(TruncatedEntrySnafu)?,
            name: Utf16Le::new(name).map_err(|_| InvalidNameRangeSnafu.build())?,
        };
        self.cursor = bytes::range(self.cursor, usize::from(length))
            .context(EntryPastValueSnafu)?
            .end;
        Ok(Some(result))
    }
}
impl<'a> Iterator for AttributeList<'a> {
    type Item = Result<AttributeListEntry<'a>, AttributeListError>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }
        let step = self.read_next();
        crate::iter::finish_step(step, &mut self.finished)
    }
}
impl FusedIterator for AttributeList<'_> {}

/// Attribute-list value, entry extent, or segment-field validation failure.
///
/// `ScanStats` mapping: `Value` delegates to [`ValueError`], including its
/// type/storage mismatch categories; other variants count as corrupt attribute lists. The core owns
/// these counters and resolves the reported segment references.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Snafu)]
#[snafu(visibility(pub(crate)))]
#[non_exhaustive]
pub enum AttributeListError {
    /// A typed attribute check failed before entry decoding.
    #[snafu(display("attribute-list value: {source}"))]
    Value {
        /// The shared type or storage-form error.
        source: ValueError,
    },
    /// The complete value exceeds 256 KiB.
    #[snafu(display("attribute-list value length {len} exceeds 262144 bytes"))]
    ValueTooLarge {
        /// Supplied byte length.
        len: usize,
    },
    /// A short tail contains nonzero bytes.
    #[snafu(display("truncated attribute-list entry"))]
    TruncatedEntry,
    /// An entry declares fewer than 26 bytes.
    #[snafu(display("invalid attribute-list entry length {length}"))]
    InvalidEntryLength {
        /// Raw entry length.
        length: u16,
    },
    /// A declared entry crosses the complete value boundary.
    #[snafu(display("attribute-list entry extends past value"))]
    EntryPastValue,
    /// The lowest VCN is negative.
    #[snafu(display("negative attribute-list lowest VCN {value}"))]
    NegativeLowestVcn {
        /// Raw signed VCN.
        value: i64,
    },
    /// A nonempty name is before the fixed header or outside the entry.
    #[snafu(display("invalid attribute-list name range"))]
    InvalidNameRange,
}
