# ntfs/runlist Specification

## Purpose
Decodes the mapping pairs of a non-resident attribute segment into runs that map virtual cluster numbers to logical cluster numbers, including sparse runs, so the scanner can locate $MFT and $BITMAP data on disk.

## Requirements

### Requirement: Segment VCN range
A runlist SHALL be decoded against the segment's declared range [lowest_vcn, highest_vcn]. lowest_vcn SHALL be in 0..=i64::MAX. highest_vcn = −1 SHALL be accepted only with lowest_vcn 0 and means an empty stream. Otherwise lowest_vcn ≤ highest_vcn < i64::MAX SHALL hold. highest_vcn = 0 SHALL mean exactly one cluster.

#### Scenario: Empty stream
- **WHEN** lowest_vcn is 0, highest_vcn is −1 and the mapping pairs begin with a zero byte
- **THEN** the runlist yields no runs and completes successfully

#### Scenario: Invalid range
- **WHEN** lowest_vcn is greater than highest_vcn + 1, or highest_vcn is −1 with lowest_vcn non-zero
- **THEN** construction or the first iteration step fails with an invalid-VCN-range error

### Requirement: Mapping pair decoding
Each mapping pair SHALL start with a header byte whose low nibble is the length-field width (1..=8) and high nibble the offset-field width (0..=8). A header of 0x00 SHALL terminate the runlist. The length SHALL be unsigned little-endian in 1..=i64::MAX. The offset SHALL be a sign-extended little-endian delta from the previous run's LCN.

#### Scenario: Single run
- **WHEN** the mapping pairs are `21 18 34 56 00` and lowest_vcn is 0
- **THEN** one run is yielded: vcn 0, length 0x18, lcn 0x5634

#### Scenario: Negative delta
- **WHEN** the first run reaches LCN 1000 and the next pair has a one-byte delta 0xF6 (−10)
- **THEN** the second run's LCN is 990

#### Scenario: Unsigned length with high bit set
- **WHEN** a pair has a one-byte length 0x80
- **THEN** the run length is 128, not negative

#### Scenario: Eight-byte fields
- **WHEN** a pair uses eight-byte length and offset fields holding valid values
- **THEN** both are decoded without overflow or a shift by 64

#### Scenario: Invalid widths
- **WHEN** a non-zero header has length width 0 or above 8, or offset width above 8
- **THEN** the iterator yields an invalid-field-widths error and then ends

### Requirement: Sparse runs
An offset width of 0 SHALL denote a sparse run with no LCN. A sparse run SHALL advance the VCN without changing the previous LCN used as the base for the next delta. An offset width above 0 with a zero delta SHALL be a physical run, and LCN 0 SHALL be representable.

#### Scenario: Physical, sparse, physical
- **WHEN** runs are physical at LCN 100, then sparse for 16 clusters, then a delta of +50
- **THEN** the third run's LCN is 150 and its VCN follows the sparse run

#### Scenario: Leading sparse run
- **WHEN** the first pair is sparse
- **THEN** it is yielded with no LCN, and the next physical delta is applied from base 0

### Requirement: Per-segment delta base
Each segment SHALL begin with a previous LCN of 0 and a starting VCN of lowest_vcn. Delta state SHALL NOT carry across segments.

#### Scenario: Extension segment
- **WHEN** a segment with lowest_vcn 500 starts with a pair whose delta is +2000
- **THEN** the first run has vcn 500 and lcn 2000, whatever the previous segment's last LCN was

### Requirement: Arithmetic and range safety
Every LCN SHALL be in 0..=i64::MAX, and every run end (LCN + length, VCN + length) SHALL not exceed i64::MAX. Overflow, underflow or a negative LCN SHALL produce an error, never a wrapped value.

#### Scenario: Negative resulting LCN
- **WHEN** the previous LCN is 5 and the delta is −6
- **THEN** the iterator yields an LCN-out-of-range error and then ends

#### Scenario: Delta overflow
- **WHEN** the previous LCN is near i64::MAX and a positive delta would exceed it
- **THEN** the iterator yields an overflow error and then ends

### Requirement: Exact coverage and termination
Runs SHALL be contiguous, starting at lowest_vcn. A run extending past highest_vcn SHALL be an error. At the zero terminator, the next VCN SHALL equal highest_vcn + 1, otherwise a coverage-mismatch error SHALL be yielded. Running out of input before a terminator SHALL be a missing-terminator error. Bytes after the terminator SHALL be ignored.

#### Scenario: Under-coverage
- **WHEN** the runs cover 10 clusters but highest_vcn − lowest_vcn + 1 is 11
- **THEN** after the last run the iterator yields a coverage-mismatch error and then ends

#### Scenario: Truncated pair
- **WHEN** the input ends inside a length or offset field
- **THEN** the iterator yields a truncated-pair error and then ends

#### Scenario: Padding after terminator
- **WHEN** arbitrary bytes follow a valid terminator
- **THEN** the decoded runs are unchanged

### Requirement: Runlist iteration is fused, bounded and allocation-free
Decoding SHALL not allocate and SHALL not merge, sort or reorder runs. Each yielded run SHALL have a positive length and consume at least two input bytes, so n bytes yield at most n / 2 runs. After an error or completion, the iterator SHALL yield nothing more.

#### Scenario: Zero-length run
- **WHEN** a pair encodes length 0
- **THEN** the iterator yields a zero-run-length error and then ends

#### Scenario: Arbitrary input
- **WHEN** arbitrary bytes are decoded under arbitrary valid VCN ranges and the iterator is exhausted
- **THEN** every call returns normally and the item count is at most n / 2 + 1

### Requirement: Optional volume bounds check
Given a volume geometry, a physical run SHALL be checked so that LCN + length ≤ total_clusters. Sparse runs SHALL have no physical extent and always pass this check.

#### Scenario: Run past volume end
- **WHEN** a physical run ends one cluster beyond total_clusters
- **THEN** the volume bounds check fails with a run-out-of-volume error
