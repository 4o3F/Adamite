# ntfs/attribute-decoding Specification

## Purpose
Decodes the NTFS attribute values the scanner consumes into typed, borrowed views: timestamps and file attributes, file names and parent links, data streams, directory index allocation, reparse tags, bitmaps and attribute-list entries. Unknown or malformed values never panic.

## Requirements

### Requirement: Typed decoders check attribute type and storage form
Each typed decoder built from an attribute SHALL verify the attribute's type code. $STANDARD_INFORMATION and $FILE_NAME SHALL require resident storage. $INDEX_ALLOCATION SHALL require non-resident storage. $DATA SHALL accept both. A wrong type or storage form SHALL return an error, not a misinterpretation.

#### Scenario: Wrong attribute type
- **WHEN** a $FILE_NAME decoder is given a $DATA attribute
- **THEN** decoding fails with a wrong-attribute-type error

#### Scenario: Wrong storage form
- **WHEN** a $STANDARD_INFORMATION decoder is given a non-resident attribute
- **THEN** decoding fails with a wrong-storage-form error

### Requirement: $STANDARD_INFORMATION decoding
The decoder SHALL accept a value of exactly 48 bytes (legacy layout) or at least 72 bytes (NTFS 3.x layout, ignoring trailing bytes). It SHALL expose four FILETIMEs as raw u64 (created 0x00, modified 0x08, record changed 0x10, accessed 0x18), file attributes u32 at 0x20 with unknown bits kept, and, for the 3.x layout only, owner id, security id, quota charged and USN.

#### Scenario: NTFS 3.x layout
- **WHEN** the value is 72 bytes
- **THEN** all four timestamps, the file attributes and the extended fields are reported

#### Scenario: Legacy layout
- **WHEN** the value is 48 bytes
- **THEN** the timestamps and file attributes are reported and the extended fields are absent

#### Scenario: Trailing bytes tolerated
- **WHEN** the value is 80 bytes
- **THEN** it decodes as the 3.x layout from the first 72 bytes

#### Scenario: Invalid length
- **WHEN** the value is shorter than 48 bytes or 49..=71 bytes long
- **THEN** decoding fails with an invalid-length error

### Requirement: $FILE_NAME decoding
The decoder SHALL require name length (u8 at 0x40) in 1..=255, namespace (u8 at 0x41) in 0..=3, and value length ≥ 0x42 + 2 × name length (trailing bytes ignored). It SHALL expose the parent reference, four raw FILETIMEs, the cached allocated and data sizes (documented as possibly stale), file attributes, the EA-size/reparse-tag word, the namespace, and the name as raw UTF-16LE.

#### Scenario: Win32 name
- **WHEN** a $FILE_NAME value has parent reference (5, 5), namespace 1 and name `notes.txt`
- **THEN** the decoded parent is record 5 sequence 5, the namespace is Win32, and the name units equal `notes.txt`

#### Scenario: Name exceeds value
- **WHEN** 0x42 + 2 × name length exceeds the value length
- **THEN** decoding fails with an invalid-name-length error

#### Scenario: Invalid namespace or empty name
- **WHEN** namespace is 4 or name length is 0
- **THEN** decoding fails with the corresponding error

### Requirement: Names are preserved as raw UTF-16
Attribute names and file names SHALL be exposed as raw little-endian UTF-16 code units, including unpaired surrogates and NUL units. The parser SHALL provide a code-unit iterator, a strict decoder that reports the first unpaired surrogate and its unit index, and a lossy decoder that replaces each decoding error with U+FFFD. Both decoders SHALL be available without `alloc`.

#### Scenario: Supplementary character
- **WHEN** a name contains U+1F600 encoded as a surrogate pair
- **THEN** strict and lossy decoding both yield the single character U+1F600

#### Scenario: Unpaired surrogate
- **WHEN** a name contains a lone 0xD800 unit followed by `a`
- **THEN** strict decoding reports an unpaired surrogate at that unit index, lossy decoding yields U+FFFD then `a`, and the raw units are unchanged

#### Scenario: Agreement with the standard decoder
- **WHEN** any sequence of u16 units is decoded
- **THEN** the strict and lossy outputs equal those of the standard library UTF-16 decoder over the same units

### Requirement: String helpers behind the alloc feature
With the `alloc` feature enabled, the parser SHALL provide strict and lossy conversions of UTF-16 names to owned strings. These SHALL produce exactly the characters of the corresponding decoders. Without the feature, these helpers SHALL NOT exist and no other API SHALL depend on allocation.

#### Scenario: Lossy string
- **WHEN** `alloc` is enabled and a name with an unpaired surrogate is converted lossily
- **THEN** the string equals the concatenated lossy decoder output

### Requirement: $DATA stream descriptors
The $DATA decoder SHALL expose the stream name (empty for the unnamed main stream), the attribute flags, and either the resident value bytes or the non-resident descriptor. Each $DATA attribute SHALL be decoded separately and never merged with another stream. Resident logical size SHALL equal the value length.

#### Scenario: Alternate data stream
- **WHEN** a record has an unnamed $DATA of 5 bytes and a named $DATA `meta` of 4 bytes
- **THEN** two separate descriptors are produced, with names empty and `meta` and sizes 5 and 4

#### Scenario: Non-resident main stream
- **WHEN** the unnamed $DATA is non-resident with lowest_vcn 0
- **THEN** the descriptor exposes allocated, data and initialized sizes and the mapping pairs

### Requirement: $INDEX_ALLOCATION descriptor
The $INDEX_ALLOCATION decoder SHALL expose the attribute name (any name, normally `$I30`), flags and non-resident descriptor. It SHALL NOT parse INDX block contents.

#### Scenario: Directory index
- **WHEN** a directory record has a non-resident $INDEX_ALLOCATION named `$I30`
- **THEN** the decoder exposes the name `$I30` and, for lowest_vcn 0, the allocated size

### Requirement: $REPARSE_POINT decoding
The decoder SHALL accept a value of 8 to 16,384 bytes whose declared data length (u16 at 0x04) satisfies 8 + data_length ≤ value length. It SHALL expose the tag (u32 at 0x00), the declared data length, and every byte after offset 8 as an opaque payload. Unknown tags SHALL be accepted. Link targets SHALL NOT be interpreted.

#### Scenario: Symbolic link tag
- **WHEN** a reparse value has tag 0xA000000C
- **THEN** the decoded tag is 0xA000000C and the payload is the bytes after offset 8

#### Scenario: Declared length too large
- **WHEN** 8 + data_length exceeds the value length
- **THEN** decoding fails with an invalid-reparse-length error

#### Scenario: Value too large
- **WHEN** the value exceeds 16,384 bytes
- **THEN** decoding fails with a reparse-value-too-large error

### Requirement: Bitmap view
A bitmap view SHALL be built from caller-supplied bytes and a logical bit count that is at most 8 × the byte length. Bit n SHALL be `(bytes[n / 8] >> (n % 8)) & 1`. Reading at or beyond the logical count SHALL return absent. Iteration SHALL yield exactly the logical number of bits, and the set-bit iterator SHALL yield strictly increasing indices.

#### Scenario: Least-significant bit first
- **WHEN** the bytes are [0b0000_0101] with logical length 8
- **THEN** bits 0 and 2 are set, and the set-bit iterator yields 0 then 2

#### Scenario: Padding bits ignored
- **WHEN** the logical length is 3 and the last byte has bits 3..7 set
- **THEN** only bits 0..3 are visible and reading bit 3 returns absent

#### Scenario: Logical length too large
- **WHEN** the logical bit count exceeds 8 × the byte length
- **THEN** construction fails with an error

### Requirement: $ATTRIBUTE_LIST entry decoding
The decoder SHALL iterate entries in a complete caller-supplied value of up to 262,144 bytes. Each entry SHALL expose its type (u32 at 0), lowest_vcn (at 8, non-negative), segment record reference (at 16), attribute id (at 24) and name. Entry length (u16 at 4) SHALL be ≥ 0x1A and within the value. A tail of fewer than 0x1A all-zero bytes SHALL end iteration. The iterator SHALL be fused after an error.

#### Scenario: $MFT data in an extension record
- **WHEN** an attribute list contains an unnamed $DATA entry with lowest_vcn 1000 and segment reference (record 17, sequence 1)
- **THEN** the iterator yields that entry with those values

#### Scenario: Truncated entry
- **WHEN** an entry's declared length extends past the end of the value
- **THEN** the iterator yields an entry-past-value error and then ends

#### Scenario: Zero-length entry
- **WHEN** an entry declares length 0
- **THEN** the iterator yields an invalid-entry-length error and then ends, without looping

### Requirement: Decoders do not apply scanner policy
Typed decoders SHALL report raw on-disk values only. They SHALL NOT choose a primary name among namespaces or hard links, substitute $FILE_NAME cached sizes for $DATA sizes, sum streams, or compute on-disk usage. That policy belongs to the scanner.

#### Scenario: Conflicting cached size
- **WHEN** a record's $FILE_NAME caches data size 0 and its unnamed $DATA reports 5000
- **THEN** each decoder reports its own value unchanged

### Requirement: Typed decoding is total
Every typed decoder SHALL return a value or an error for arbitrary input bytes. It SHALL never panic, overflow, read out of bounds, or allocate, except for the alloc-gated string helpers.

#### Scenario: Arbitrary value bytes
- **WHEN** arbitrary bytes from 0 to 65,536 long are passed to each typed decoder and every returned iterator is exhausted
- **THEN** every call returns normally
