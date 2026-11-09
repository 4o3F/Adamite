//! Versioned independent NTFS manifest schema and decimal-string measurements.

use serde::{Deserialize, Deserializer};

fn decimal<'de, D: Deserializer<'de>>(input: D) -> Result<u64, D::Error> {
    String::deserialize(input)?
        .parse()
        .map_err(serde::de::Error::custom)
}
fn optional_decimal<'de, D: Deserializer<'de>>(input: D) -> Result<Option<u64>, D::Error> {
    Option::<String>::deserialize(input)?
        .map(|value| value.parse().map_err(serde::de::Error::custom))
        .transpose()
}

#[derive(Debug, Deserialize)]
#[serde(try_from = "ManifestWire")]
/// Independent schema-v2 or schema-v3 oracle for one compressed NTFS image.
pub struct Manifest {
    /// Manifest schema version, 2 or 3.
    pub schema_version: u32,
    /// Sibling zstd image filename.
    pub image: String,
    /// Expected decompressed image length in bytes.
    pub image_len: u64,
    /// Tool names and versions recorded during generation.
    pub generator: std::collections::BTreeMap<String, String>,
    /// Expected boot-sector geometry.
    pub geometry: Geometry,
    /// Expected MFT stream size and mapping.
    pub mft: Mft,
    /// Explicit filesystem entries, including separate hard-link paths.
    pub entries: Vec<Entry>,
    /// Independently enumerated NTFS metadata records, empty for v2.
    pub metafiles: Vec<Entry>,
    /// Compact summaries of generated filler files.
    pub fill: Vec<Fill>,
    /// Deleted paths and their former record numbers.
    pub deleted: Vec<Deleted>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
/// Independent summary of filler files under one directory.
pub struct Fill {
    /// Directory path containing the filler files.
    pub dir: String,
    /// Number of distinct filler files.
    pub count: usize,
    /// Expected name prefix.
    pub name_prefix: String,
    /// Expected name length in UTF-16 code units.
    pub name_len: usize,
    #[serde(deserialize_with = "decimal")]
    /// Smallest filler record number.
    pub record_min: u64,
    #[serde(deserialize_with = "decimal")]
    /// Largest filler record number.
    pub record_max: u64,
    #[serde(deserialize_with = "decimal")]
    /// Expected parent record number.
    pub parent_record: u64,
    /// Expected parent sequence number.
    pub parent_seq: u16,
    /// Sum of unnamed DATA bytes before filler compaction; absent in v2.
    #[serde(default, deserialize_with = "optional_decimal")]
    pub total_size: Option<u64>,
    /// Sum of named DATA bytes before filler compaction; absent in v2.
    #[serde(default, deserialize_with = "optional_decimal")]
    pub total_ads_size: Option<u64>,
    /// Sum of physical stream bytes before filler compaction; absent in v2.
    #[serde(default, deserialize_with = "optional_decimal")]
    pub total_alloc: Option<u64>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
/// Expected geometry measured by ntfsinfo.
pub struct Geometry {
    /// Logical sector size in bytes.
    pub bytes_per_sector: u16,
    /// Cluster size in bytes.
    pub cluster_size: u32,
    /// FILE record size in bytes.
    pub file_record_size: u32,
    /// Index record size in bytes.
    pub index_record_size: u32,
    #[serde(deserialize_with = "decimal")]
    /// Number of complete volume clusters.
    pub total_clusters: u64,
    #[serde(deserialize_with = "decimal")]
    /// Physical cluster containing MFT record zero.
    pub mft_lcn: u64,
    #[serde(deserialize_with = "decimal")]
    /// Physical cluster containing the MFT mirror.
    pub mftmirr_lcn: u64,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
/// Expected mapping and logical length of the MFT DATA stream.
pub struct Mft {
    /// Expected extents in virtual-cluster order.
    pub extents: Vec<Extent>,
    #[serde(deserialize_with = "decimal")]
    /// Logical MFT data size in bytes.
    pub data_size: u64,
    /// Whether record zero contains an attribute list.
    pub has_attribute_list: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
/// One virtual-cluster extent with an optional physical location.
pub struct Extent {
    #[serde(deserialize_with = "decimal")]
    /// Starting virtual cluster number.
    pub vcn: u64,
    #[serde(deserialize_with = "optional_decimal")]
    /// Starting physical cluster number, absent for a sparse run.
    pub lcn: Option<u64>,
    #[serde(deserialize_with = "decimal")]
    /// Length in clusters.
    pub len: u64,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
/// Independent file metadata shared by every path to the same base record.
pub struct Entry {
    /// Absolute volume-relative path.
    pub path: String,
    #[serde(deserialize_with = "decimal")]
    /// Base MFT record number.
    pub record: u64,
    /// Base record sequence number.
    pub seq: u16,
    /// Entry kind measured independently.
    pub kind: EntryKind,
    /// Reported hard-link count.
    pub nlink: u64,
    /// Raw STANDARD_INFORMATION file attributes.
    pub attributes: u32,
    #[serde(deserialize_with = "decimal")]
    /// Modified time as a raw FILETIME.
    pub mtime_filetime: u64,
    /// All FILE_NAME links for this record.
    pub names: Vec<Name>,
    /// DATA and non-resident attribute streams after joining continuation segments.
    pub streams: Vec<Stream>,
    /// Reparse tag when present.
    pub reparse_tag: Option<u32>,
    /// Whether the record has an attribute list.
    pub has_attribute_list: bool,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
/// Filesystem entry kinds represented by the fixture manifests.
pub enum EntryKind {
    /// A directory.
    Directory,
    /// An ordinary file.
    File,
    /// A symbolic link.
    Symlink,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
/// One FILE_NAME link retaining its original UTF-16 units.
pub struct Name {
    #[serde(deserialize_with = "decimal")]
    /// Parent directory record number.
    pub parent_record: u64,
    /// Parent directory sequence number.
    pub parent_seq: u16,
    /// Original UTF-16 code units.
    pub name: Vec<u16>,
    /// Raw NTFS namespace value.
    pub namespace: u8,
}
/// One DATA or non-resident attribute stream measured across its segments.
#[derive(Debug, Deserialize)]
#[serde(from = "StreamWire")]
pub struct Stream {
    /// Raw attribute type, defaulting to DATA for v2.
    pub attribute_type: u32,
    /// Stream name as UTF-16 units; empty for the main stream.
    pub name: Vec<u16>,
    /// Logical size in bytes.
    pub size: u64,
    /// First-segment allocated size; absent in v2.
    pub allocated_size: Option<u64>,
    /// First-segment physical size for sparse/compressed storage; absent otherwise.
    pub compressed_size: Option<u64>,
    /// Whether the stream is stored in the FILE record.
    pub resident: bool,
    /// Whether the sparse attribute flag is set.
    pub sparse: bool,
    /// Whether a compression format is set.
    pub compressed: bool,
    /// Total mapping-pair runs across every segment.
    pub run_count: usize,
    v3_measurements_present: bool,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
/// A deleted path whose record must remain out of the live tree.
pub struct Deleted {
    /// Former volume-relative path.
    pub path: String,
    #[serde(deserialize_with = "decimal")]
    /// Former MFT record number.
    pub record: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestWire {
    /// Manifest schema version, 2 or 3.
    schema_version: u32,
    /// Sibling zstd image filename.
    image: String,
    #[serde(deserialize_with = "decimal")]
    /// Expected decompressed image length in bytes.
    image_len: u64,
    /// Tool names and versions recorded during generation.
    generator: std::collections::BTreeMap<String, String>,
    /// Expected boot-sector geometry.
    geometry: Geometry,
    /// Expected MFT stream size and mapping.
    mft: Mft,
    /// Explicit filesystem entries, including separate hard-link paths.
    entries: Vec<Entry>,
    /// Independently enumerated NTFS metadata records, empty for v2.
    #[serde(default)]
    metafiles: Option<Vec<Entry>>,
    /// Compact summaries of generated filler files.
    fill: Vec<Fill>,
    /// Deleted paths and their former record numbers.
    deleted: Vec<Deleted>,
}

impl TryFrom<ManifestWire> for Manifest {
    type Error = &'static str;
    fn try_from(wire: ManifestWire) -> Result<Self, Self::Error> {
        if wire.schema_version == 3 && wire.metafiles.is_none() {
            return Err("v3 requires metafiles");
        }
        let manifest = Self {
            schema_version: wire.schema_version,
            image: wire.image,
            image_len: wire.image_len,
            generator: wire.generator,
            geometry: wire.geometry,
            mft: wire.mft,
            entries: wire.entries,
            metafiles: wire.metafiles.unwrap_or_default(),
            fill: wire.fill,
            deleted: wire.deleted,
        };
        manifest.validate()?;
        Ok(manifest)
    }
}

impl Manifest {
    pub(crate) fn validate(&self) -> Result<(), &'static str> {
        if !matches!(self.schema_version, 2 | 3) {
            return Err("unsupported manifest schema");
        }
        if self.schema_version == 2 {
            return Ok(());
        }
        for entry in self.entries.iter().chain(&self.metafiles) {
            for stream in &entry.streams {
                if !stream.v3_measurements_present
                    || stream.allocated_size.is_none()
                    || stream.attribute_type == 0
                {
                    return Err("v3 requires stream type and allocation measurements");
                }
                if stream.resident {
                    if stream.attribute_type != 0x80
                        || stream.allocated_size != Some(0)
                        || stream.compressed_size.is_some()
                        || stream.run_count != 0
                    {
                        return Err("invalid resident v3 stream measurements");
                    }
                } else if (stream.sparse || stream.compressed) != stream.compressed_size.is_some() {
                    return Err("invalid non-resident v3 compressed size");
                }
            }
        }
        if self.fill.iter().any(|fill| {
            fill.total_size.is_none() || fill.total_ads_size.is_none() || fill.total_alloc.is_none()
        }) {
            return Err("v3 requires filler totals");
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StreamWire {
    #[serde(default)]
    attribute_type: Option<u32>,
    name: Vec<u16>,
    #[serde(deserialize_with = "decimal")]
    size: u64,
    #[serde(default, deserialize_with = "optional_decimal")]
    allocated_size: Option<u64>,
    #[serde(default, deserialize_with = "present_decimal")]
    compressed_size: Option<Option<u64>>,
    resident: bool,
    sparse: bool,
    compressed: bool,
    run_count: usize,
}

fn present_decimal<'de, D: Deserializer<'de>>(input: D) -> Result<Option<Option<u64>>, D::Error> {
    optional_decimal(input).map(Some)
}

impl From<StreamWire> for Stream {
    fn from(wire: StreamWire) -> Self {
        Self {
            v3_measurements_present: wire.attribute_type.is_some()
                && wire.allocated_size.is_some()
                && wire.compressed_size.is_some(),
            attribute_type: wire.attribute_type.unwrap_or(0x80),
            name: wire.name,
            size: wire.size,
            allocated_size: wire.allocated_size,
            compressed_size: wire.compressed_size.flatten(),
            resident: wire.resident,
            sparse: wire.sparse,
            compressed: wire.compressed,
            run_count: wire.run_count,
        }
    }
}
