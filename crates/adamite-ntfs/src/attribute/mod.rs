//! Borrowed attribute headers and non-resident descriptor validation.

pub mod iter;
pub mod non_resident;
pub mod runlist;

use snafu::{OptionExt, Snafu, ensure};

use self::non_resident::{NonResident, StreamSizes};
use self::runlist::VcnRange;
use crate::{utf16::Utf16Le, util};

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
        let type_code = util::read_u32(input, 0).context(TruncatedHeaderSnafu)?;
        ensure!(type_code != 0, InvalidTypeSnafu);
        let length = util::read_u32(input, 4).context(TruncatedHeaderSnafu)?;
        let form = util::read_u8(input, 8).context(TruncatedHeaderSnafu)?;
        ensure!(form <= 1, InvalidStorageFormSnafu { value: form });
        let minimum = if form == 0 { 24 } else { 64 };
        let length_usize = usize::try_from(length)
            .ok()
            .context(InvalidLengthSnafu { length })?;
        ensure!(
            length_usize >= minimum && util::is_aligned(length_usize, 8),
            InvalidLengthSnafu { length }
        );
        let bytes = input
            .get(..length_usize)
            .context(AttributePastUsedBytesSnafu)?;
        let flags =
            AttributeFlags::from_raw(util::read_u16(bytes, 12).context(TruncatedHeaderSnafu)?);
        let header_size = if form == 1 && flags.extended_header() {
            ensure!(length_usize >= 72, MissingExtendedHeaderSnafu);
            72
        } else {
            minimum
        };
        let name_count = util::read_u8(bytes, 9).context(TruncatedHeaderSnafu)?;
        let name = if name_count == 0 {
            &[][..]
        } else {
            let offset = usize::from(util::read_u16(bytes, 10).context(TruncatedHeaderSnafu)?);
            ensure!(offset >= header_size, InvalidNameRangeSnafu);
            let range = util::scaled_range(offset, usize::from(name_count), 2)
                .context(InvalidNameRangeSnafu)?;
            bytes.get(range).context(InvalidNameRangeSnafu)?
        };
        let body = if form == 0 {
            let offset = usize::from(util::read_u16(bytes, 20).context(TruncatedHeaderSnafu)?);
            let len = util::read_u32(bytes, 16)
                .and_then(|value| usize::try_from(value).ok())
                .context(InvalidValueRangeSnafu)?;
            let range = util::range(offset, len).context(InvalidValueRangeSnafu)?;
            Body::Resident(bytes.get(range).context(InvalidValueRangeSnafu)?)
        } else {
            Body::NonResident(NonResident::parse(bytes, flags)?)
        };
        Ok(Self {
            bytes,
            type_code: AttributeType(type_code),
            id: util::read_u16(bytes, 14).context(TruncatedHeaderSnafu)?,
            flags,
            name: Utf16Le::new(name).map_err(|_| InvalidNameRangeSnafu.build())?,
            body,
        })
    }
}

impl<'a> NonResident<'a> {
    fn parse(input: &'a [u8], flags: AttributeFlags) -> Result<Self, AttributeError> {
        let offset = usize::from(util::read_u16(input, 0x20).context(TruncatedHeaderSnafu)?);
        let header_size = if flags.extended_header() { 72 } else { 64 };
        ensure!(offset >= header_size, InvalidMappingPairsSnafu);
        let mapping_pairs = input.get(offset..).context(InvalidMappingPairsSnafu)?;
        let lowest = util::read_i64(input, 0x10).context(TruncatedHeaderSnafu)?;
        let highest = util::read_i64(input, 0x18).context(TruncatedHeaderSnafu)?;
        let range = VcnRange::new(lowest, highest).map_err(|_| InvalidVcnRangeSnafu.build())?;
        let sizes = if lowest == 0 {
            let allocated = read_size(input, 0x28)?;
            let data = read_size(input, 0x30)?;
            let initialized = read_size(input, 0x38)?;
            let compressed = if flags.extended_header() {
                Some(read_size(input, 0x40)?)
            } else {
                None
            };
            ensure!(initialized <= data, InitializedPastDataSnafu);
            Some(StreamSizes {
                allocated,
                data,
                initialized,
                compressed,
            })
        } else {
            None
        };
        Ok(Self {
            range,
            compression_unit: util::read_u8(input, 0x22).context(TruncatedHeaderSnafu)?,
            mapping_pairs,
            sizes,
        })
    }
}

fn read_size(input: &[u8], offset: usize) -> Result<u64, AttributeError> {
    let value = util::read_i64(input, offset).context(TruncatedHeaderSnafu)?;
    u64::try_from(value)
        .ok()
        .context(NegativeSizeSnafu { offset })
}

/// Attribute header, extent or descriptor validation failure.
///
/// `ScanStats` mapping: every variant counts as a corrupt attribute. The core
/// owns this counter; iterator errors are reported once before termination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Snafu)]
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
