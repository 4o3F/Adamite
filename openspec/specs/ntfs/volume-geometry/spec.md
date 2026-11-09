# ntfs/volume-geometry Specification

## Purpose
Decodes and validates an NTFS boot sector into the volume geometry the scanner needs to locate the MFT and convert cluster numbers into byte offsets, without performing any I/O.

## Requirements

### Requirement: Boot sector identification
The parser SHALL accept a boot sector only if the input is at least 512 bytes, bytes 0x03..0x0B equal `NTFS` followed by four spaces, and bytes 0x1FE..0x200 equal `0x55 0xAA`. Only the first 512 bytes SHALL be interpreted. Boot code, CHS fields, reserved fields and checksum SHALL be ignored.

#### Scenario: Valid boot sector with trailing bytes
- **WHEN** a valid 512-byte NTFS boot sector is followed by arbitrary suffix bytes
- **THEN** the decoded geometry equals the geometry decoded from the 512-byte prefix alone

#### Scenario: Truncated input
- **WHEN** the input is shorter than 512 bytes
- **THEN** parsing fails with a truncation error

#### Scenario: Wrong OEM identifier or end signature
- **WHEN** any byte of the OEM identifier or the `0x55AA` end signature differs
- **THEN** parsing fails with an identification error

### Requirement: Sector and cluster size decoding
`bytes_per_sector` (u16 at 0x0B) SHALL be one of 512, 1024, 2048 or 4096. `sectors_per_cluster` (u8 at 0x0D) SHALL be a literal power of two from 1 to 128, or an exponent byte 0xF4..=0xFF meaning 2^(256−value) sectors. The resulting cluster size SHALL be at most 2 MiB (2,097,152 bytes).

#### Scenario: Literal sectors per cluster
- **WHEN** bytes_per_sector is 512 and sectors_per_cluster is 8
- **THEN** the cluster size is 4096 bytes

#### Scenario: Exponent-encoded large cluster
- **WHEN** bytes_per_sector is 512 and sectors_per_cluster is 0xF8
- **THEN** the cluster size is 512 × 2^8 = 131,072 bytes

#### Scenario: Maximum cluster size
- **WHEN** bytes_per_sector is 512 and sectors_per_cluster is 0xF4
- **THEN** the cluster size is 2,097,152 bytes and parsing succeeds

#### Scenario: Cluster size above the limit
- **WHEN** bytes_per_sector is 4096 and sectors_per_cluster is 0xF4
- **THEN** parsing fails because the cluster size exceeds 2 MiB

#### Scenario: Invalid sector or cluster encodings
- **WHEN** bytes_per_sector is not in {512, 1024, 2048, 4096}, or sectors_per_cluster is 0, a non-power-of-two value up to 0x80, or a value in 0x81..=0xF3
- **THEN** parsing fails with the corresponding sector or cluster error

### Requirement: File and index record size decoding
`clusters_per_file_record` (i8 at 0x40) and `clusters_per_index_record` (i8 at 0x44) SHALL decode as: positive n gives n × cluster size; negative −k gives 2^k bytes; zero is invalid. Each resulting size SHALL be a power of two from 512 to 65,536 bytes inclusive.

#### Scenario: Negative encoding
- **WHEN** clusters_per_file_record is 0xF6 (−10) and the cluster size is 4096
- **THEN** the file record size is 1024 bytes

#### Scenario: Positive encoding with small clusters
- **WHEN** clusters_per_file_record is 2 and the cluster size is 512
- **THEN** the file record size is 1024 bytes

#### Scenario: Zero or out-of-range encoding
- **WHEN** the encoded value is 0 or −128, or decodes to a size outside 512..=65536 or to a non-power of two
- **THEN** parsing fails with a record size error identifying whether the file or the index record size was invalid, without arithmetic overflow

### Requirement: Volume extent and MFT location
The parser SHALL expose `total_sectors` (u64 at 0x28), `mft_lcn` (u64 at 0x30), `mftmirr_lcn` (u64 at 0x38) and the volume serial (u64 at 0x48). `total_sectors` SHALL be at least 1, at most `i64::MAX`, and its byte size SHALL fit in a u64. `total_clusters` SHALL be `floor(total_sectors / sectors_per_cluster)` and at least 1. MFT and MFTMirr LCNs SHALL each leave room for one file record inside the volume.

#### Scenario: Valid MFT location
- **WHEN** mft_lcn × cluster size + file record size is at most total_clusters × cluster size
- **THEN** parsing succeeds and exposes mft_lcn unchanged

#### Scenario: MFT beyond volume end
- **WHEN** mft_lcn or mftmirr_lcn is greater than or equal to total_clusters
- **THEN** parsing fails with an address-out-of-volume error

#### Scenario: Zero or overflowing sector count
- **WHEN** total_sectors is 0, or total_sectors × bytes_per_sector overflows u64
- **THEN** parsing fails with an invalid total sectors error

### Requirement: Cluster byte range conversion
Given a parsed geometry, converting an LCN and cluster count to a byte range SHALL return `[lcn × cluster_size, (lcn + count) × cluster_size)` only when count ≥ 1, no arithmetic overflows, and the range lies within complete volume clusters. Otherwise it SHALL return an error.

#### Scenario: Range inside the volume
- **WHEN** lcn + count ≤ total_clusters
- **THEN** the returned range starts at lcn × cluster_size and has length count × cluster_size

#### Scenario: Range past the end or overflowing
- **WHEN** lcn + count > total_clusters, count is 0, or the multiplication overflows u64
- **THEN** conversion fails with an error and no range is returned

### Requirement: Geometry parsing is total
For every input byte sequence, geometry parsing SHALL return either a geometry or an error. It SHALL never panic, overflow, or read outside the input.

#### Scenario: Arbitrary bytes
- **WHEN** an arbitrary byte sequence of any length, including empty, is parsed
- **THEN** the call returns normally with a geometry or an error
