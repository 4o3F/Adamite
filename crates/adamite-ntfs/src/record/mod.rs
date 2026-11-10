//! FILE record header views and record classification.

pub mod fixup;
pub mod reference;
pub mod system;

use snafu::{OptionExt, ResultExt, Snafu, ensure};

use self::fixup::FixupError;
use self::reference::FileRef;
use crate::{attribute::iter::AttributeIter, util};

/// Record flags retaining every unknown on-disk bit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RecordFlags(u16);

impl RecordFlags {
    /// The record is allocated and in use.
    pub const IN_USE: Self = Self(0x0001);
    /// The record describes a directory.
    pub const DIRECTORY: Self = Self(0x0002);
    /// Preserves the complete u16 at record offset 0x16.
    #[must_use]
    pub const fn from_raw(raw: u16) -> Self {
        Self(raw)
    }
    /// Returns all flag bits from record offset 0x16.
    #[must_use]
    pub const fn raw(self) -> u16 {
        self.0
    }
    /// Reports whether every bit in `flags` is present.
    #[must_use]
    pub const fn contains(self, flags: Self) -> bool {
        self.0 & flags.0 == flags.0
    }
}

/// Eagerly decoded fixed fields preceding the first update-sequence protected tail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordHeader {
    signature: [u8; 4],
    usa_offset: u16,
    usa_count: u16,
    lsn: u64,
    sequence: u16,
    hard_links: u16,
    first_attribute_offset: u16,
    flags: RecordFlags,
    bytes_used: u32,
    bytes_allocated: u32,
    base_reference: FileRef,
    next_attribute_id: u16,
    record_number: Option<u32>,
}

impl RecordHeader {
    /// Reads fixed fields without validating the signature or applying fixup.
    ///
    /// # Errors
    ///
    /// Returns [`RecordError::TruncatedHeader`] below 0x2A bytes, or below 0x30
    /// bytes when the USA offset selects the NTFS 3.1 layout.
    pub fn peek(input: &[u8]) -> Result<Self, RecordError> {
        let header = input.get(..0x2a).context(TruncatedHeaderSnafu)?;
        let usa_offset = util::read_u16(header, 4).context(TruncatedHeaderSnafu)?;
        let record_number = if usa_offset >= 0x30 {
            Some(util::read_u32(input, 0x2c).context(TruncatedHeaderSnafu)?)
        } else {
            None
        };
        Ok(Self {
            signature: header
                .get(..4)
                .and_then(|value| value.try_into().ok())
                .context(TruncatedHeaderSnafu)?,
            usa_offset,
            usa_count: util::read_u16(header, 6).context(TruncatedHeaderSnafu)?,
            lsn: util::read_u64(header, 8).context(TruncatedHeaderSnafu)?,
            sequence: util::read_u16(header, 0x10).context(TruncatedHeaderSnafu)?,
            hard_links: util::read_u16(header, 0x12).context(TruncatedHeaderSnafu)?,
            first_attribute_offset: util::read_u16(header, 0x14).context(TruncatedHeaderSnafu)?,
            flags: RecordFlags::from_raw(
                util::read_u16(header, 0x16).context(TruncatedHeaderSnafu)?,
            ),
            bytes_used: util::read_u32(header, 0x18).context(TruncatedHeaderSnafu)?,
            bytes_allocated: util::read_u32(header, 0x1c).context(TruncatedHeaderSnafu)?,
            base_reference: FileRef::from_raw(
                util::read_u64(header, 0x20).context(TruncatedHeaderSnafu)?,
            ),
            next_attribute_id: util::read_u16(header, 0x28).context(TruncatedHeaderSnafu)?,
            record_number,
        })
    }

    /// Returns the four signature bytes at offset 0x00.
    #[must_use]
    pub const fn signature(self) -> [u8; 4] {
        self.signature
    }
    /// Returns the update-sequence array offset at 0x04.
    #[must_use]
    pub const fn usa_offset(self) -> u16 {
        self.usa_offset
    }
    /// Returns the update-sequence array count at 0x06.
    #[must_use]
    pub const fn usa_count(self) -> u16 {
        self.usa_count
    }
    /// Returns the log sequence number at 0x08.
    #[must_use]
    pub const fn lsn(self) -> u64 {
        self.lsn
    }
    /// Returns the reuse sequence number at 0x10, including zero.
    #[must_use]
    pub const fn sequence(self) -> u16 {
        self.sequence
    }
    /// Returns the hard-link count at 0x12, including zero.
    #[must_use]
    pub const fn hard_links(self) -> u16 {
        self.hard_links
    }
    /// Returns the first attribute offset at 0x14.
    #[must_use]
    pub const fn first_attribute_offset(self) -> u16 {
        self.first_attribute_offset
    }
    /// Returns all flags at 0x16, including unknown bits.
    #[must_use]
    pub const fn flags(self) -> RecordFlags {
        self.flags
    }
    /// Returns the used byte count at 0x18.
    #[must_use]
    pub const fn bytes_used(self) -> u32 {
        self.bytes_used
    }
    /// Returns the allocated byte count at 0x1C.
    #[must_use]
    pub const fn bytes_allocated(self) -> u32 {
        self.bytes_allocated
    }
    /// Returns the base reference at 0x20; only an all-zero reference means base.
    #[must_use]
    pub const fn base_reference(self) -> FileRef {
        self.base_reference
    }
    /// Returns the next attribute instance identifier at 0x28.
    #[must_use]
    pub const fn next_attribute_id(self) -> u16 {
        self.next_attribute_id
    }
    /// Returns the stored record number at 0x2C for the NTFS 3.1 layout only.
    #[must_use]
    pub const fn record_number(self) -> Option<u32> {
        self.record_number
    }

    fn validate(self, len: usize) -> Result<(), RecordError> {
        ensure!(
            self.signature == *b"FILE",
            InvalidSignatureSnafu {
                signature: self.signature
            }
        );
        ensure!(
            usize::try_from(self.bytes_allocated).ok() == Some(len),
            InvalidAllocatedSizeSnafu {
                declared: self.bytes_allocated,
                actual: len
            }
        );
        let used = usize::try_from(self.bytes_used)
            .ok()
            .context(InvalidUsedSizeSnafu {
                used: self.bytes_used,
                allocated: self.bytes_allocated,
            })?;
        ensure!(
            self.bytes_used <= self.bytes_allocated && util::is_aligned(used, 8),
            InvalidUsedSizeSnafu {
                used: self.bytes_used,
                allocated: self.bytes_allocated
            }
        );
        let first = usize::from(self.first_attribute_offset);
        let minimum = util::range(first, 8).context(InvalidFirstAttributeSnafu {
            offset: self.first_attribute_offset,
            used: self.bytes_used,
        })?;
        ensure!(
            util::is_aligned(first, 8) && minimum.end <= used,
            InvalidFirstAttributeSnafu {
                offset: self.first_attribute_offset,
                used: self.bytes_used
            }
        );
        ensure!(
            fixup::valid_usa_offset(self.usa_offset),
            InvalidUsaOffsetSnafu {
                offset: self.usa_offset
            }
        );
        Ok(())
    }
}

fn validate_size(len: usize) -> Result<(), RecordError> {
    ensure!(
        fixup::valid_record_size(len),
        InvalidRecordSizeSnafu { len }
    );
    Ok(())
}

/// A validated FILE record borrowing bytes restored by successful update-sequence fixup.
///
/// A live record view prevents mutating its backing buffer:
///
/// ```compile_fail,E0499
/// use adamite_ntfs::record::FileRecord;
///
/// let mut buffer = [0u8; 1024];
/// let record = FileRecord::parse(&mut buffer)?;
/// buffer.fill(0);
/// let header = record.header();
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug)]
pub struct FileRecord<'a> {
    bytes: &'a [u8],
    header: RecordHeader,
}

impl<'a> FileRecord<'a> {
    /// Validates the FILE header, restores protected trailers, and borrows the record.
    ///
    /// The buffer remains unchanged on every error. Valid headers are accepted
    /// regardless of IN_USE; successful parsing applies fixup once.
    ///
    /// # Examples
    ///
    /// ```
    /// use adamite_ntfs::record::{FileRecord, RecordError};
    ///
    /// let mut buffer = [0u8; 1024];
    /// let before = buffer;
    /// assert!(matches!(FileRecord::parse(&mut buffer), Err(RecordError::InvalidSignature { .. })));
    /// assert_eq!(buffer, before);
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`RecordError`] in order of input size, signature, declared byte
    /// counts, attribute offset, update-sequence offset, then fixup metadata and
    /// protected trailers. Fixup failures are wrapped in [`RecordError::Fixup`].
    /// The shared size and USA-offset rules report [`RecordError::InvalidRecordSize`]
    /// and [`RecordError::InvalidUsaOffset`], respectively.
    pub fn parse(input: &'a mut [u8]) -> Result<Self, RecordError> {
        validate_size(input.len())?;
        let header = Self::validate_header(input)?;
        Self::from_validated_header(input, header)
    }

    fn validate_header(input: &[u8]) -> Result<RecordHeader, RecordError> {
        let header = RecordHeader::peek(input)?;
        header.validate(input.len())?;
        Ok(header)
    }

    fn from_validated_header(
        input: &'a mut [u8],
        header: RecordHeader,
    ) -> Result<Self, RecordError> {
        let fixed = fixup::apply(input).context(FixupSnafu)?;
        Ok(Self {
            bytes: fixed.as_bytes(),
            header,
        })
    }

    /// Returns the fixed header decoded when this view was created.
    #[must_use]
    pub const fn header(&self) -> RecordHeader {
        self.header
    }

    /// Borrows the complete fixed-up record, including attribute bytes and slack.
    #[must_use]
    pub const fn as_bytes(&self) -> &'a [u8] {
        self.bytes
    }

    /// Iterates attributes only within `[first_attribute_offset, bytes_used)`.
    ///
    /// Exhaust the iterator before accepting the record: corruption can be
    /// reported after earlier attributes have already been yielded.
    pub fn attributes(&self) -> AttributeIter<'a> {
        let region = usize::try_from(self.header.bytes_used())
            .ok()
            .and_then(|end| {
                self.as_bytes()
                    .get(usize::from(self.header.first_attribute_offset())..end)
            });
        AttributeIter::new(region)
    }
}

/// Classification result, with borrowed views for valid in-use records.
#[derive(Debug)]
#[non_exhaustive]
pub enum RecordClass<'a> {
    /// Every byte is zero or every byte is 0xFF.
    Unused,
    /// The signature is the chkdsk marker `BAAD`.
    Baad,
    /// A valid header has IN_USE clear; its buffer was not fixed up.
    NotInUse(RecordHeader),
    /// An in-use record has an all-zero base reference and was fixed up.
    Base(FileRecord<'a>),
    /// An in-use record has a non-null base reference and was fixed up.
    Extension(FileRecord<'a>),
    /// Validation or fixup failed; its buffer was not changed.
    Corrupt(RecordError),
}

impl<'a> RecordClass<'a> {
    /// Classifies a buffer and fixes up only valid in-use FILE records.
    ///
    /// Precedence is size, uniform fill, BAAD, FILE signature, header validity,
    /// IN_USE, fixup, then null/non-null base reference. Every result other than
    /// `Base` or `Extension` leaves the buffer byte-for-byte unchanged.
    pub fn classify(input: &'a mut [u8]) -> Self {
        match Self::classify_inner(input) {
            Ok(class) => class,
            Err(error) => Self::Corrupt(error),
        }
    }

    fn classify_inner(input: &'a mut [u8]) -> Result<Self, RecordError> {
        validate_size(input.len())?;
        if input.iter().all(|&byte| byte == 0) || input.iter().all(|&byte| byte == 0xff) {
            return Ok(Self::Unused);
        }
        if input.get(..4) == Some(b"BAAD") {
            return Ok(Self::Baad);
        }
        let header = FileRecord::validate_header(input)?;
        if !header.flags.contains(RecordFlags::IN_USE) {
            return Ok(Self::NotInUse(header));
        }
        let record = FileRecord::from_validated_header(input, header)?;
        Ok(if header.base_reference.is_null() {
            Self::Base(record)
        } else {
            Self::Extension(record)
        })
    }
}

/// FILE header or classification failure.
///
/// `ScanStats` mapping: `Fixup` follows the nested [`FixupError`] category;
/// `InvalidSignature` counts as an invalid signature. Other variants count as
/// corrupt record headers. The core owns these counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Snafu)]
#[snafu(visibility(pub(crate)))]
#[non_exhaustive]
pub enum RecordError {
    /// The input does not contain all fixed fields for its selected layout.
    #[snafu(display("truncated FILE record header"))]
    TruncatedHeader,
    /// Record size is not a power of two in 512..=65536.
    #[snafu(display("invalid FILE record size {len}"))]
    InvalidRecordSize {
        /// Supplied byte length.
        len: usize,
    },
    /// The header signature is not FILE.
    #[snafu(display("invalid FILE record signature {signature:?}"))]
    InvalidSignature {
        /// Raw four-byte signature.
        signature: [u8; 4],
    },
    /// The allocation field disagrees with the supplied record length.
    #[snafu(display("allocated record bytes {declared} differ from buffer length {actual}"))]
    InvalidAllocatedSize {
        /// Header allocation field.
        declared: u32,
        /// Supplied buffer length.
        actual: usize,
    },
    /// Used bytes exceed allocation or are not 8-aligned.
    #[snafu(display("used record bytes {used} are invalid for allocation {allocated}"))]
    InvalidUsedSize {
        /// Header used-byte field.
        used: u32,
        /// Header allocation field.
        allocated: u32,
    },
    /// The first attribute is unaligned or lacks eight bytes within bytes_used.
    #[snafu(display("first attribute offset {offset} is invalid for {used} used bytes"))]
    InvalidFirstAttribute {
        /// Header attribute offset.
        offset: u16,
        /// Header used-byte field.
        used: u32,
    },
    /// The update-sequence offset is odd or precedes 0x2A.
    #[snafu(display("invalid FILE update-sequence offset {offset}"))]
    InvalidUsaOffset {
        /// Header update-sequence offset.
        offset: u16,
    },
    /// Update-sequence fixup failed after the header was validated.
    #[snafu(display("FILE update-sequence fixup failed: {source}"))]
    Fixup {
        /// Owned Copy parser error from fixup.
        source: FixupError,
    },
}
