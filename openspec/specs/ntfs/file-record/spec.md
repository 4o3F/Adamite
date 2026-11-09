# ntfs/file-record Specification

## Purpose
Decodes NTFS FILE records from caller-owned MFT bytes: file references, header fields, update-sequence fixup, record classification, and safe iteration over the record's attributes, so the scanner can process millions of records without allocation.

## Requirements

### Requirement: File reference encoding
A file reference SHALL be a 64-bit value whose low 48 bits are the record number and high 16 bits are the sequence number. Decoding from a raw u64 or 8 little-endian bytes SHALL be lossless for every value. Constructing from parts SHALL reject record numbers ≥ 2^48 instead of truncating them.

#### Scenario: Raw round trip
- **WHEN** any u64 value x is converted to a file reference and back
- **THEN** the result equals x, record() equals x & (2^48 − 1), and sequence() equals x >> 48

#### Scenario: Construction bounds
- **WHEN** a reference is constructed from record 2^48 − 1 and any sequence
- **THEN** construction succeeds and both parts round-trip
- **WHEN** a reference is constructed from record 2^48
- **THEN** construction fails with a record-out-of-range error

#### Scenario: Null reference
- **WHEN** the raw value is 0
- **THEN** the reference reports itself as null; a reference with record 0 and a non-zero sequence is not null

### Requirement: Well-known record numbers
The parser SHALL define constants for the NTFS system records: $MFT 0, $MFTMirr 1, $LogFile 2, $Volume 3, $AttrDef 4, root directory 5, $Bitmap 6, $Boot 7, $BadClus 8, $Secure 9, $UpCase 10, $Extend 11, and the first non-reserved record number 16.

#### Scenario: Root constant
- **WHEN** the root directory constant is read
- **THEN** its record number is 5

### Requirement: Record header peek before fixup
The parser SHALL decode the signature, flags, sequence number, hard-link count, base reference, bytes used, bytes allocated, first attribute offset and update-sequence offset/count from a record buffer without applying fixup and without mutating it. These fields all lie before offset 0x30, ahead of the first protected sector tail.

#### Scenario: Peek matches post-fixup header
- **WHEN** a valid protected FILE record is peeked, then fixed up and parsed
- **THEN** the peeked signature, flags, sequence, base reference and offsets equal the parsed header's fields

#### Scenario: Peek on short input
- **WHEN** the buffer is shorter than the fixed header
- **THEN** peek fails with a truncated-header error

### Requirement: Header layout detection
A FILE record SHALL be treated as NTFS 3.1 layout when its update-sequence offset is ≥ 0x30, and as NTFS 3.0 layout otherwise. Only the 3.1 layout SHALL expose the stored record number (u32 at 0x2C). For 3.0 records the stored record number SHALL be reported as absent.

#### Scenario: NTFS 3.1 record
- **WHEN** the update-sequence offset is 0x30 and bytes 0x2C..0x30 hold 42
- **THEN** the stored record number is reported as 42

#### Scenario: NTFS 3.0 record
- **WHEN** the update-sequence offset is 0x2A and bytes 0x2C..0x30 hold arbitrary data
- **THEN** the stored record number is reported as absent and those bytes do not affect any other decoded field

### Requirement: Record header validation
A FILE record buffer SHALL have a length that is a power of two from 512 to 65,536. Its header SHALL satisfy: bytes_allocated equals the buffer length; bytes_used ≤ bytes_allocated and is 8-aligned; first_attribute_offset is 8-aligned and first_attribute_offset + 8 ≤ bytes_used; the update-sequence offset is even and ≥ 0x2A.

#### Scenario: Valid header
- **WHEN** a 1024-byte record has bytes_allocated 1024, bytes_used 416 and first_attribute_offset 56
- **THEN** header validation succeeds

#### Scenario: Inconsistent sizes
- **WHEN** bytes_allocated differs from the buffer length, or bytes_used exceeds bytes_allocated, or either offset is misaligned
- **THEN** validation fails with the specific header error and the buffer is unchanged

### Requirement: Update sequence fixup
Fixup SHALL operate in place on a caller-owned mutable buffer with a fixed 512-byte stride, whatever the volume sector size. The update-sequence count SHALL equal buffer_length / 512 + 1. The array SHALL end at or before offset 510 and at or before first_attribute_offset. Every sector tail SHALL match the update sequence number before any tail is restored. Any update sequence number value, including 0 and 0xFFFF, SHALL be accepted.

#### Scenario: Successful fixup of a 1 KiB record
- **WHEN** a 1024-byte record has update-sequence count 3 and both tails at 510 and 1022 equal the update sequence number
- **THEN** the tails are replaced by array entries 1 and 2, and no other byte changes

#### Scenario: 4 KiB record on a 4Kn volume
- **WHEN** a 4096-byte record is fixed up
- **THEN** the expected update-sequence count is 9 and eight tails at 512-byte strides are restored

#### Scenario: Torn write
- **WHEN** any sector tail differs from the update sequence number
- **THEN** fixup fails with a trailer-mismatch error identifying the first mismatching stride, and the buffer is byte-identical to its input

#### Scenario: Bad update-sequence metadata
- **WHEN** the count differs from buffer_length / 512 + 1, the offset is odd or below 0x2A, or the array overlaps offset 510 or the first attribute
- **THEN** fixup fails with the specific metadata error and the buffer is unchanged

### Requirement: Fixed records are parsed exactly once
Successful fixup SHALL produce a fixed-record token, which only the fixup operation can construct. Full record parsing and attribute iteration SHALL require this token. All views derived from it SHALL borrow the caller's buffer, so the buffer cannot be mutated or recycled while any view is alive.

#### Scenario: Parsing requires fixup
- **WHEN** a caller wants to iterate a record's attributes
- **THEN** the only way to obtain an attribute iterator from a record buffer is through a successful fixup of that buffer

#### Scenario: Views borrow the buffer
- **WHEN** an attribute view obtained from a fixed record is still in use
- **THEN** the buffer cannot be mutably borrowed, and code that tries to do so is rejected at compile time

### Requirement: Record classification
Classifying a record buffer SHALL return exactly one of Unused, Baad, NotInUse, Base, Extension or Corrupt. Only Base and Extension apply fixup. Every other class leaves the buffer unchanged.

#### Scenario: Never-used record
- **WHEN** the buffer is all zero bytes
- **THEN** it is classified Unused without mutation; a buffer that is entirely 0xFF is also Unused

#### Scenario: Chkdsk-marked record
- **WHEN** the signature is `BAAD`
- **THEN** it is classified Baad without fixup or further header interpretation

#### Scenario: Base record
- **WHEN** an in-use FILE record has a null base reference and passes fixup
- **THEN** it is classified Base and the buffer has been fixed up

#### Scenario: Unknown signature
- **WHEN** the signature is neither `FILE` nor `BAAD`, and the buffer is not uniformly 0x00 or 0xFF
- **THEN** it is classified Corrupt with an invalid-signature error, and the buffer is unchanged

#### Scenario: Deleted record
- **WHEN** a FILE record has a valid header and the IN_USE flag clear
- **THEN** it is classified NotInUse, exposes its header, and the buffer is not mutated

#### Scenario: Extension record
- **WHEN** an in-use FILE record has base reference (record 1234, sequence 7)
- **THEN** it is classified Extension, exposes that base reference, and the buffer has been fixed up

#### Scenario: Base reference with record 0 and non-zero sequence
- **WHEN** an in-use FILE record's base reference has record 0 and sequence 3
- **THEN** it is classified Extension, because only the all-zero reference means base

#### Scenario: Torn in-use record
- **WHEN** an in-use FILE record fails fixup
- **THEN** it is classified Corrupt with the fixup error, and the buffer is unchanged

### Requirement: Unknown header data is preserved
Unknown record flag bits, a zero sequence number and a zero hard-link count SHALL be preserved and reported. They SHALL NOT be rejected.

#### Scenario: Unknown flags
- **WHEN** a record has flags 0x0001 | 0x0004 | 0x0100
- **THEN** it is in use, and the raw flags value reported is 0x0105

### Requirement: Attribute iteration bounds
Iteration SHALL cover only `[first_attribute_offset, bytes_used)` and SHALL stop at a type value of 0xFFFFFFFF. Each attribute SHALL be 8-aligned in length, at least 0x18 bytes when resident or 0x40 when non-resident, and contained in the region. Reaching the end of the region without the end marker SHALL be an error. Bytes after the end marker or beyond bytes_used SHALL never be read.

#### Scenario: Normal record
- **WHEN** a record contains $STANDARD_INFORMATION, $FILE_NAME and $DATA followed by the end marker
- **THEN** the iterator yields those three attributes in order and then ends

#### Scenario: Attribute crossing bytes_used
- **WHEN** an attribute's declared length extends past bytes_used, even though the bytes exist in the buffer
- **THEN** the iterator yields an error and then ends

#### Scenario: Missing end marker
- **WHEN** the attributes exactly fill the region with no end marker
- **THEN** after the last attribute the iterator yields a missing-end-marker error and then ends

#### Scenario: Slack does not matter
- **WHEN** bytes after the end marker or after bytes_used are changed arbitrarily
- **THEN** the iterator's output is unchanged

### Requirement: Attribute iteration terminates
Each successful iteration step SHALL advance by at least 24 bytes. After yielding an error, the iterator SHALL yield nothing more. An iterator over a region of n bytes SHALL therefore yield at most n / 24 + 1 items.

#### Scenario: Zero-length attribute
- **WHEN** an attribute declares length 0
- **THEN** the iterator yields one invalid-length error and then ends, without looping

#### Scenario: Fused after error
- **WHEN** next() is called repeatedly after the iterator has yielded an error or ended
- **THEN** every further call returns no item

### Requirement: Attribute header decoding
Each attribute SHALL expose:
- its raw type code;
- its id;
- its raw flags (COMPRESSED 0x0001 mask 0x00FF, ENCRYPTED 0x4000, SPARSE 0x8000; unknown bits kept);
- its name as raw UTF-16LE (empty when the name length is 0);
- whether it is resident.

A type code of 0 SHALL be an error. Unknown non-zero type codes SHALL be yielded unchanged. A non-resident flag other than 0 or 1 SHALL be an error. A non-empty name SHALL lie within the attribute after its fixed header.

#### Scenario: Unknown attribute type
- **WHEN** a record contains an attribute with type 0x100 between known attributes
- **THEN** the iterator yields it with raw type 0x100 and continues to the next attribute

#### Scenario: Named attribute
- **WHEN** a $DATA attribute has name length 4 and the name `meta`
- **THEN** the name view yields the UTF-16 code units of `meta`

#### Scenario: Name out of bounds
- **WHEN** name_offset + 2 × name_length exceeds the attribute length
- **THEN** the iterator yields an invalid-name-range error and then ends

### Requirement: Resident attribute values
A resident attribute SHALL expose its value as a borrowed byte slice `[value_offset, value_offset + value_length)`, which SHALL lie within the attribute. An empty value SHALL be valid.

#### Scenario: Resident value
- **WHEN** a resident attribute has value_offset 0x18 and value_length 72
- **THEN** the exposed value is exactly those 72 bytes

#### Scenario: Value out of bounds
- **WHEN** value_offset + value_length exceeds the attribute length
- **THEN** the iterator yields an invalid-value-range error and then ends

### Requirement: Non-resident attribute descriptors
A non-resident attribute SHALL expose:
- lowest_vcn and highest_vcn;
- compression_unit as the single u8 at 0x22;
- its mapping-pairs bytes, from mapping_pairs_offset to the end of the attribute.

When the compression format bits are non-zero or SPARSE is set, the attribute SHALL be at least 0x48 bytes and compressed_size (at 0x40) SHALL be present.

#### Scenario: Compressed stream header
- **WHEN** a non-resident attribute has the COMPRESSED flag and length 0x48 or more
- **THEN** its compressed size is reported from offset 0x40

#### Scenario: Short header for a compressed stream
- **WHEN** a non-resident attribute is compressed or sparse but shorter than 0x48 bytes
- **THEN** the iterator yields a missing-extended-header error and then ends

#### Scenario: Reserved bytes after compression unit
- **WHEN** bytes 0x23..0x28 hold arbitrary values
- **THEN** the reported compression unit equals byte 0x22 alone

### Requirement: Non-resident sizes only from the first segment
Allocated, data, initialized and compressed sizes SHALL be reported only when lowest_vcn is 0, and SHALL be absent otherwise, whatever those bytes contain. When reported, sizes SHALL be non-negative as i64, with initialized_size ≤ data_size. The parser SHALL NOT enforce data_size ≤ allocated_size.

#### Scenario: Continuation segment
- **WHEN** a non-resident attribute has lowest_vcn 300 and arbitrary bytes in its size fields
- **THEN** its sizes are reported as absent and the attribute is otherwise valid

#### Scenario: Negative size in first segment
- **WHEN** lowest_vcn is 0 and data_size has its sign bit set
- **THEN** the iterator yields a negative-size error and then ends

### Requirement: Parsing is total and allocation-free
Every record and attribute operation SHALL return normally for arbitrary input, without panic, overflow or out-of-bounds reads. Peek, fixup, classification and attribute iteration SHALL NOT allocate heap memory, and SHALL be usable without the `alloc` feature.

#### Scenario: Arbitrary record bytes
- **WHEN** arbitrary bytes of any accepted record size are classified and every resulting iterator is exhausted
- **THEN** every call returns normally with a value or an error

#### Scenario: No allocation in the hot path
- **WHEN** a valid fixture record is classified and all its attributes are iterated under a counting allocator
- **THEN** the allocation count does not change
