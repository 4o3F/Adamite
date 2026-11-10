//! Borrowed attribute headers and bounded iteration over fixed FILE records.

use core::iter::FusedIterator;

use snafu::{OptionExt, Snafu, ensure};

use crate::{FileRecord, Utf16Le, bytes};

mod attribute_list;
mod bitmap;
mod data;
mod index_allocation;
mod nonresident;
pub use attribute_list::{AttributeList, AttributeListEntry, AttributeListError};
mod reparse_point;
pub use bitmap::{Bitmap, BitmapError, BitmapIter};
pub use data::Data;
pub use index_allocation::IndexAllocation;
pub use nonresident::{NonResident, StreamSizes};
pub use reparse_point::ReparsePoint;
mod file_name;
mod standard_information;
pub use file_name::FileName;
pub use standard_information::{FileAttributes, FileTimes, StandardInformation};

/// Attribute type code retaining unknown nonzero values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AttributeType(u32);

impl AttributeType {
    /// `$STANDARD_INFORMATION` timestamps and flags.
    pub const STANDARD_INFORMATION: Self = Self(0x10);
    /// `$ATTRIBUTE_LIST` entries referring to attribute segments.
    pub const ATTRIBUTE_LIST: Self = Self(0x20);
    /// `$FILE_NAME` cached metadata and a directory name.
    pub const FILE_NAME: Self = Self(0x30);
    /// `$DATA` stream contents or extents.
    pub const DATA: Self = Self(0x80);
    /// `$INDEX_ROOT` resident index root.
    pub const INDEX_ROOT: Self = Self(0x90);
    /// `$INDEX_ALLOCATION` non-resident index blocks.
    pub const INDEX_ALLOCATION: Self = Self(0xa0);
    /// `$BITMAP` allocation bits.
    pub const BITMAP: Self = Self(0xb0);
    /// `$REPARSE_POINT` tag and opaque data.
    pub const REPARSE_POINT: Self = Self(0xc0);
    /// Preserves an on-disk type code.
    #[must_use]
    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }
    /// Returns the original type code.
    #[must_use]
    pub const fn raw(self) -> u32 {
        self.0
    }
}

/// Attribute flags retaining compression format and all unknown bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AttributeFlags(u16);

impl AttributeFlags {
    /// The default compressed format bit.
    pub const COMPRESSED: Self = Self(0x0001);
    /// Mask covering the complete compression format byte.
    pub const COMPRESSION_MASK: Self = Self(0x00ff);
    /// The stream is encrypted.
    pub const ENCRYPTED: Self = Self(0x4000);
    /// The stream has sparse extents.
    pub const SPARSE: Self = Self(0x8000);
    /// Preserves every on-disk flag bit.
    #[must_use]
    pub const fn from_raw(raw: u16) -> Self {
        Self(raw)
    }
    /// Returns every original flag bit.
    #[must_use]
    pub const fn raw(self) -> u16 {
        self.0
    }
    /// Reports whether every requested flag bit is present.
    #[must_use]
    pub const fn contains(self, flags: Self) -> bool {
        self.0 & flags.0 == flags.0
    }
    /// Returns the complete compression format byte.
    #[must_use]
    pub const fn compression_format(self) -> u8 {
        (self.0 & 0xff) as u8
    }

    fn extended_header(self) -> bool {
        self.compression_format() != 0 || self.contains(Self::SPARSE)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Body<'a> {
    Resident(&'a [u8]),
    NonResident(NonResident<'a>),
}

/// Decoded fixed fields and borrowed variable data for one complete attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Attribute<'a> {
    bytes: &'a [u8],
    type_code: AttributeType,
    id: u16,
    flags: AttributeFlags,
    name: Utf16Le<'a>,
    body: Body<'a>,
}

impl<'a> Attribute<'a> {
    /// Returns the original complete attribute, including header and padding.
    #[must_use]
    pub const fn as_bytes(self) -> &'a [u8] {
        self.bytes
    }
    /// Returns the raw-preserving type code at offset 0x00.
    #[must_use]
    pub const fn attribute_type(self) -> AttributeType {
        self.type_code
    }
    /// Returns the attribute instance identifier at offset 0x0E.
    #[must_use]
    pub const fn id(self) -> u16 {
        self.id
    }
    /// Returns all flags from offset 0x0C.
    #[must_use]
    pub const fn flags(self) -> AttributeFlags {
        self.flags
    }
    /// Returns the original name units, including NULs and unpaired surrogates.
    #[must_use]
    pub const fn name(self) -> Utf16Le<'a> {
        self.name
    }
    /// Reports whether the storage flag at offset 0x08 is resident.
    #[must_use]
    pub const fn is_resident(self) -> bool {
        matches!(self.body, Body::Resident(_))
    }
    /// Borrows the declared resident value, including an empty value.
    #[must_use]
    pub const fn resident_value(self) -> Option<&'a [u8]> {
        match self.body {
            Body::Resident(value) => Some(value),
            Body::NonResident(_) => None,
        }
    }

    /// Returns the validated non-resident segment descriptor when present.
    #[must_use]
    pub const fn non_resident(self) -> Option<NonResident<'a>> {
        match self.body {
            Body::Resident(_) => None,
            Body::NonResident(descriptor) => Some(descriptor),
        }
    }

    fn parse(input: &'a [u8]) -> Result<Self, AttributeError> {
        let type_code = bytes::read_u32(input, 0).context(TruncatedHeaderSnafu)?;
        ensure!(type_code != 0, InvalidTypeSnafu);
        let length = bytes::read_u32(input, 4).context(TruncatedHeaderSnafu)?;
        let form = bytes::read_u8(input, 8).context(TruncatedHeaderSnafu)?;
        ensure!(form <= 1, InvalidStorageFormSnafu { value: form });
        let minimum = if form == 0 { 24 } else { 64 };
        let length_usize = usize::try_from(length)
            .ok()
            .context(InvalidLengthSnafu { length })?;
        ensure!(
            length_usize >= minimum && bytes::is_aligned(length_usize, 8),
            InvalidLengthSnafu { length }
        );
        let bytes = input
            .get(..length_usize)
            .context(AttributePastUsedBytesSnafu)?;
        let flags =
            AttributeFlags::from_raw(bytes::read_u16(bytes, 12).context(TruncatedHeaderSnafu)?);
        let header_size = if form == 1 && flags.extended_header() {
            ensure!(length_usize >= 72, MissingExtendedHeaderSnafu);
            72
        } else {
            minimum
        };
        let name_count = bytes::read_u8(bytes, 9).context(TruncatedHeaderSnafu)?;
        let name = if name_count == 0 {
            &[][..]
        } else {
            let offset = usize::from(bytes::read_u16(bytes, 10).context(TruncatedHeaderSnafu)?);
            ensure!(offset >= header_size, InvalidNameRangeSnafu);
            let range = bytes::scaled_range(offset, usize::from(name_count), 2)
                .context(InvalidNameRangeSnafu)?;
            bytes.get(range).context(InvalidNameRangeSnafu)?
        };
        let body = if form == 0 {
            let offset = usize::from(bytes::read_u16(bytes, 20).context(TruncatedHeaderSnafu)?);
            let len = bytes::read_u32(bytes, 16)
                .and_then(|value| usize::try_from(value).ok())
                .context(InvalidValueRangeSnafu)?;
            let range = bytes::range(offset, len).context(InvalidValueRangeSnafu)?;
            Body::Resident(bytes.get(range).context(InvalidValueRangeSnafu)?)
        } else {
            Body::NonResident(NonResident::parse(bytes, flags)?)
        };
        Ok(Self {
            bytes,
            type_code: AttributeType(type_code),
            id: bytes::read_u16(bytes, 14).context(TruncatedHeaderSnafu)?,
            flags,
            name: Utf16Le::new(name).map_err(|_| InvalidNameRangeSnafu.build())?,
            body,
        })
    }
}

/// Fused attribute iterator bounded to the record's declared used-byte region.
/// Exhaust the iterator to validate every header and the end marker.
#[must_use = "exhaust the attribute iterator to validate the complete record"]
#[derive(Debug, Clone)]
pub struct AttributeIter<'a> {
    region: Option<&'a [u8]>,
    cursor: usize,
    finished: bool,
}

impl<'a> AttributeIter<'a> {
    /// Borrows an arbitrary attribute region in instrumented fuzz builds.
    #[cfg(fuzzing)]
    pub fn from_region(region: &'a [u8]) -> Self {
        Self {
            region: Some(region),
            cursor: 0,
            finished: false,
        }
    }

    pub(crate) fn new(record: &FileRecord<'a>) -> Self {
        let header = record.header();
        let region = usize::try_from(header.bytes_used()).ok().and_then(|end| {
            record
                .as_bytes()
                .get(usize::from(header.first_attribute_offset())..end)
        });
        Self {
            region,
            cursor: 0,
            finished: false,
        }
    }

    fn read_next(&mut self) -> Result<Option<Attribute<'a>>, AttributeError> {
        let input = self
            .region
            .context(InvalidRegionSnafu)?
            .get(self.cursor..)
            .context(InvalidRegionSnafu)?;
        let kind = bytes::read_u32(input, 0).context(MissingEndMarkerSnafu)?;
        if kind == u32::MAX {
            return Ok(None);
        }
        let attribute = Attribute::parse(input)?;
        self.cursor = bytes::range(self.cursor, attribute.bytes.len())
            .context(AttributePastUsedBytesSnafu)?
            .end;
        Ok(Some(attribute))
    }
}

impl<'a> Iterator for AttributeIter<'a> {
    type Item = Result<Attribute<'a>, AttributeError>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }
        let step = self.read_next();
        crate::iter::finish_step(step, &mut self.finished)
    }
}
impl FusedIterator for AttributeIter<'_> {}

fn ensure_type(attribute: Attribute<'_>, expected: AttributeType) -> Result<(), ValueError> {
    ensure!(
        attribute.attribute_type() == expected,
        WrongAttributeTypeSnafu {
            expected: expected.raw(),
            actual: attribute.attribute_type().raw()
        }
    );
    Ok(())
}

fn resident_value(attribute: Attribute<'_>, expected: AttributeType) -> Result<&[u8], ValueError> {
    ensure_type(attribute, expected)?;
    attribute.resident_value().context(WrongStorageFormSnafu)
}

/// Typed attribute payload validation failure.
///
/// `ScanStats` mapping: `WrongAttributeType` and `WrongStorageForm` count as
/// attribute type/storage mismatches; other variants count as corrupt typed
/// values. The core owns these counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Snafu)]
#[snafu(visibility(pub(crate)))]
#[non_exhaustive]
pub enum ValueError {
    /// The attribute type differs from the requested typed decoder.
    #[snafu(display("attribute type {actual:#x} differs from expected {expected:#x}"))]
    WrongAttributeType {
        /// Required type code.
        expected: u32,
        /// Supplied type code.
        actual: u32,
    },
    /// The attribute uses a storage form unsupported by this decoder.
    #[snafu(display("unsupported attribute storage form"))]
    WrongStorageForm,
    /// A fixed field is incomplete.
    #[snafu(display("truncated typed attribute value"))]
    TruncatedValue,
    /// Standard information is neither 48 bytes nor at least 72 bytes.
    #[snafu(display("invalid standard-information length {len}"))]
    InvalidStandardInformationLength {
        /// Supplied value length.
        len: usize,
    },
    /// The file name is empty or its declared units exceed the value.
    #[snafu(display("invalid file-name length"))]
    InvalidNameLength,
    /// The file-name namespace is outside 0..=3.
    #[snafu(display("invalid file-name namespace {namespace}"))]
    InvalidNamespace {
        /// Raw namespace byte.
        namespace: u8,
    },
    /// A reparse value's declared data extends beyond its supplied bytes.
    #[snafu(display("invalid reparse data length"))]
    InvalidReparseLength,
    /// A reparse value exceeds 16 KiB.
    #[snafu(display("reparse value length {len} exceeds 16384 bytes"))]
    ReparseValueTooLarge {
        /// Supplied value length.
        len: usize,
    },
}

/// Attribute header, extent or descriptor validation failure.
///
/// `ScanStats` mapping: every variant counts as a corrupt attribute. The core
/// owns this counter; iterator errors are reported once before termination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Snafu)]
#[snafu(visibility(pub(crate)))]
#[non_exhaustive]
pub enum AttributeError {
    /// The fixed header is incomplete.
    #[snafu(display("truncated attribute header"))]
    TruncatedHeader,
    /// The used-byte region ended without the type end marker.
    #[snafu(display("missing attribute end marker"))]
    MissingEndMarker,
    /// The record's used-byte region is outside its buffer.
    #[snafu(display("invalid attribute region"))]
    InvalidRegion,
    /// Attribute type zero is reserved and cannot be iterated.
    #[snafu(display("invalid zero attribute type"))]
    InvalidType,
    /// Storage flag is neither zero nor one.
    #[snafu(display("invalid attribute storage flag {value}"))]
    InvalidStorageForm {
        /// Raw storage flag.
        value: u8,
    },
    /// Declared length is too small or not a multiple of eight.
    #[snafu(display("invalid attribute length {length}"))]
    InvalidLength {
        /// Raw length field.
        length: u32,
    },
    /// The complete attribute does not fit within bytes_used.
    #[snafu(display("attribute extends past used record bytes"))]
    AttributePastUsedBytes,
    /// Compressed or sparse storage lacks the 72-byte header.
    #[snafu(display("missing extended non-resident attribute header"))]
    MissingExtendedHeader,
    /// A nonempty name lies before its fixed header or past the attribute.
    #[snafu(display("invalid attribute name range"))]
    InvalidNameRange,
    /// The declared resident value lies outside the attribute.
    #[snafu(display("invalid resident value range"))]
    InvalidValueRange,
    /// Mapping pairs begin before the fixed header or past the attribute.
    #[snafu(display("invalid mapping-pairs offset"))]
    InvalidMappingPairs,
    /// The signed segment VCN range is invalid.
    #[snafu(display("invalid non-resident VCN range"))]
    InvalidVcnRange,
    /// A first-segment size has its sign bit set.
    #[snafu(display("negative stream size at attribute offset {offset:#x}"))]
    NegativeSize {
        /// Offset of the invalid signed size.
        offset: usize,
    },
    /// Initialized bytes exceed the logical data length.
    #[snafu(display("initialized stream size exceeds data size"))]
    InitializedPastData,
}
