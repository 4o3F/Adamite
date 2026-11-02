#![allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]
//! Reference MFT bootstrap behavior for adamite-core Phase 0, using committed image bytes.

use adamite_ntfs::{
    AttributeList, AttributeType, FileRecord, FileRef, NonResident, VolumeGeometry, fixup,
    parse_boot_sector,
};
use ruzstd::decoding::StreamingDecoder;
use serde::{Deserialize, Deserializer};
use snafu::{OptionExt, ResultExt, Snafu, ensure};
use std::{
    collections::BTreeSet,
    fs,
    io::{self, Read},
    path::Path,
};

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
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema_version: u32,
    pub image: String,
    #[serde(deserialize_with = "decimal")]
    pub image_len: u64,
    pub generator: std::collections::BTreeMap<String, String>,
    pub geometry: Geometry,
    pub mft: Mft,
    pub entries: Vec<Entry>,
    pub fill: Vec<Fill>,
    pub deleted: Vec<Deleted>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fill {
    pub dir: String,
    pub count: usize,
    pub name_prefix: String,
    pub name_len: usize,
    #[serde(deserialize_with = "decimal")]
    pub record_min: u64,
    #[serde(deserialize_with = "decimal")]
    pub record_max: u64,
    #[serde(deserialize_with = "decimal")]
    pub parent_record: u64,
    pub parent_seq: u16,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Geometry {
    pub bytes_per_sector: u16,
    pub cluster_size: u32,
    pub file_record_size: u32,
    pub index_record_size: u32,
    #[serde(deserialize_with = "decimal")]
    pub total_clusters: u64,
    #[serde(deserialize_with = "decimal")]
    pub mft_lcn: u64,
    #[serde(deserialize_with = "decimal")]
    pub mftmirr_lcn: u64,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mft {
    pub extents: Vec<Extent>,
    #[serde(deserialize_with = "decimal")]
    pub data_size: u64,
    pub has_attribute_list: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Extent {
    #[serde(deserialize_with = "decimal")]
    pub vcn: u64,
    #[serde(deserialize_with = "optional_decimal")]
    pub lcn: Option<u64>,
    #[serde(deserialize_with = "decimal")]
    pub len: u64,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub path: String,
    #[serde(deserialize_with = "decimal")]
    pub record: u64,
    pub seq: u16,
    pub kind: EntryKind,
    pub nlink: u64,
    pub attributes: u32,
    #[serde(deserialize_with = "decimal")]
    pub mtime_filetime: u64,
    pub names: Vec<Name>,
    pub streams: Vec<Stream>,
    pub reparse_tag: Option<u32>,
    pub has_attribute_list: bool,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    Directory,
    File,
    Symlink,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Name {
    #[serde(deserialize_with = "decimal")]
    pub parent_record: u64,
    pub parent_seq: u16,
    pub name: Vec<u16>,
    pub namespace: u8,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stream {
    pub name: Vec<u16>,
    #[serde(deserialize_with = "decimal")]
    pub size: u64,
    pub resident: bool,
    pub sparse: bool,
    pub compressed: bool,
    pub run_count: usize,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Deleted {
    pub path: String,
    #[serde(deserialize_with = "decimal")]
    pub record: u64,
}

#[derive(Debug, Snafu)]
pub enum FixtureError {
    #[snafu(display("fixture I/O failed: {source}"))]
    Io { source: io::Error },
    #[snafu(display("invalid fixture manifest: {source}"))]
    Json { source: serde_json::Error },
    #[snafu(display("invalid fixture: {message}"))]
    Invalid { message: String },
}
type Result<T, E = FixtureError> = std::result::Result<T, E>;
fn invalid(error: impl std::fmt::Display) -> FixtureError {
    InvalidSnafu {
        message: error.to_string(),
    }
    .build()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Request {
    record: u64,
    sequence: u16,
    id: u16,
    lowest: u64,
}

pub struct Fixture {
    pub manifest: Manifest,
    pub geometry: VolumeGeometry,
    pub extents: Vec<Extent>,
    image: Vec<u8>,
    mft: Vec<u8>,
}

impl Fixture {
    pub fn load(manifest_path: &Path) -> Result<Self> {
        let manifest: Manifest = serde_json::from_slice(&fs::read(manifest_path).context(IoSnafu)?)
            .context(JsonSnafu)?;
        let directory = manifest_path.parent().context(InvalidSnafu {
            message: "manifest has no parent",
        })?;
        ensure!(
            Path::new(&manifest.image)
                .file_name()
                .and_then(|name| name.to_str())
                == Some(manifest.image.as_str()),
            InvalidSnafu {
                message: "image must be a sibling filename"
            }
        );
        let source = fs::File::open(directory.join(&manifest.image)).context(IoSnafu)?;
        let mut decoder = StreamingDecoder::new(source).map_err(invalid)?;
        let mut image = Vec::new();
        let limit = manifest.image_len.checked_add(1).context(InvalidSnafu {
            message: "image length overflow",
        })?;
        decoder
            .by_ref()
            .take(limit)
            .read_to_end(&mut image)
            .context(IoSnafu)?;
        ensure!(
            u64::try_from(image.len()).ok() == Some(manifest.image_len),
            InvalidSnafu {
                message: "decompressed image_len differs from manifest"
            }
        );
        let checksum = decoder.decoder.get_checksum_from_data();
        ensure!(
            checksum.is_some() && checksum == decoder.decoder.get_calculated_checksum(),
            InvalidSnafu {
                message: "missing or mismatched zstd checksum"
            }
        );
        Self::from_image(image, manifest)
    }

    pub fn from_image(image: Vec<u8>, manifest: Manifest) -> Result<Self> {
        ensure!(
            manifest.schema_version == 2
                && u64::try_from(image.len()).ok() == Some(manifest.image_len),
            InvalidSnafu {
                message: "schema version or image length mismatch"
            }
        );
        let geometry = parse_boot_sector(&image).map_err(invalid)?;
        let expected = &manifest.geometry;
        ensure!(
            geometry.bytes_per_sector() == expected.bytes_per_sector
                && geometry.cluster_size() == expected.cluster_size
                && geometry.file_record_size() == expected.file_record_size
                && geometry.index_record_size() == expected.index_record_size
                && geometry.total_clusters() == expected.total_clusters
                && geometry.mft_lcn() == expected.mft_lcn
                && geometry.mftmirr_lcn() == expected.mftmirr_lcn,
            InvalidSnafu {
                message: "geometry differs from independent manifest"
            }
        );
        let start = geometry
            .mft_lcn()
            .checked_mul(u64::from(geometry.cluster_size()))
            .context(InvalidSnafu {
                message: "MFT address overflow",
            })?;
        let mut raw = physical(&image, start, u64::from(geometry.file_record_size()))?.to_vec();
        let record =
            FileRecord::parse(fixup::apply(&mut raw).map_err(invalid)?).map_err(invalid)?;
        ensure!(
            record.header().base_reference().is_null(),
            InvalidSnafu {
                message: "record zero is an extension"
            }
        );
        let sequence = record.header().sequence();
        let mut known = Vec::new();
        let mut requests = Vec::new();
        let mut visited = BTreeSet::new();
        let mut data_size = None;
        for attribute in record.attributes() {
            let attribute = attribute.map_err(invalid)?;
            if attribute.attribute_type() == AttributeType::DATA
                && attribute.name().len_units() == 0
            {
                let descriptor = attribute.non_resident().context(InvalidSnafu {
                    message: "resident MFT DATA",
                })?;
                if let Some(sizes) = descriptor.sizes() {
                    data_size = Some(sizes.data());
                }
                add_runs(&mut known, descriptor, &geometry)?;
                visited.insert(Request {
                    record: 0,
                    sequence,
                    id: attribute.id(),
                    lowest: descriptor.lowest_vcn(),
                });
            }
            if attribute.attribute_type() == AttributeType::ATTRIBUTE_LIST {
                let value = if let Some(value) = attribute.resident_value() {
                    value.to_vec()
                } else {
                    let descriptor = attribute.non_resident().context(InvalidSnafu {
                        message: "missing attribute-list descriptor",
                    })?;
                    let size = descriptor
                        .sizes()
                        .context(InvalidSnafu {
                            message: "attribute-list first segment missing",
                        })?
                        .data();
                    ensure!(
                        size <= 262144,
                        InvalidSnafu {
                            message: "attribute list exceeds 256 KiB"
                        }
                    );
                    let mut runs = Vec::new();
                    add_runs(&mut runs, descriptor, &geometry)?;
                    read_virtual(&image, &geometry, &runs, 0, size)?.context(InvalidSnafu {
                        message: "incomplete attribute-list extents",
                    })?
                };
                for entry in AttributeList::new(&value).map_err(invalid)? {
                    let entry = entry.map_err(invalid)?;
                    if entry.attribute_type() == AttributeType::DATA
                        && entry.name().len_units() == 0
                    {
                        requests.push(Request {
                            record: entry.segment_reference().record(),
                            sequence: entry.segment_reference().sequence(),
                            id: entry.attribute_id(),
                            lowest: entry.lowest_vcn(),
                        });
                    }
                }
            }
        }
        let data_size = data_size.context(InvalidSnafu {
            message: "MFT first DATA segment missing",
        })?;
        while !requests.is_empty() {
            let mut pending = Vec::new();
            let mut progress = false;
            for request in requests {
                if visited.contains(&request) {
                    continue;
                }
                let offset = request
                    .record
                    .checked_mul(u64::from(geometry.file_record_size()))
                    .context(InvalidSnafu {
                        message: "extension record address overflow",
                    })?;
                ensure!(
                    offset
                        .checked_add(u64::from(geometry.file_record_size()))
                        .is_some_and(|end| end <= data_size),
                    InvalidSnafu {
                        message: "extension record exceeds MFT data size"
                    }
                );
                let Some(mut raw) = read_virtual(
                    &image,
                    &geometry,
                    &known,
                    offset,
                    u64::from(geometry.file_record_size()),
                )?
                else {
                    pending.push(request);
                    continue;
                };
                let record =
                    FileRecord::parse(fixup::apply(&mut raw).map_err(invalid)?).map_err(invalid)?;
                ensure!(
                    record.header().sequence() == request.sequence
                        && record.header().base_reference()
                            == FileRef::new(0, sequence).map_err(invalid)?,
                    InvalidSnafu {
                        message: "MFT extension reference or sequence mismatch"
                    }
                );
                let mut found = false;
                for attribute in record.attributes() {
                    let attribute = attribute.map_err(invalid)?;
                    if attribute.attribute_type() == AttributeType::DATA
                        && attribute.name().len_units() == 0
                        && attribute.id() == request.id
                    {
                        let descriptor = attribute.non_resident().context(InvalidSnafu {
                            message: "resident MFT continuation",
                        })?;
                        ensure!(
                            descriptor.lowest_vcn() == request.lowest,
                            InvalidSnafu {
                                message: "MFT continuation VCN mismatch"
                            }
                        );
                        add_runs(&mut known, descriptor, &geometry)?;
                        found = true;
                    }
                }
                ensure!(
                    found,
                    InvalidSnafu {
                        message: "attribute-list target not found"
                    }
                );
                visited.insert(request);
                progress = true;
            }
            ensure!(
                pending.is_empty() || progress,
                InvalidSnafu {
                    message: "MFT bootstrap cannot reach listed records through known extents"
                }
            );
            requests = pending;
        }
        known.sort_by_key(|extent| extent.vcn);
        let mut end = 0;
        for extent in &known {
            ensure!(
                extent.vcn == end,
                InvalidSnafu {
                    message: "MFT extents overlap or have a gap"
                }
            );
            end = end.checked_add(extent.len).context(InvalidSnafu {
                message: "MFT coverage overflow",
            })?;
        }
        ensure!(
            data_size == manifest.mft.data_size && known == manifest.mft.extents,
            InvalidSnafu {
                message: "MFT differs from independent manifest"
            }
        );
        let mft = read_virtual(&image, &geometry, &known, 0, data_size)?.context(InvalidSnafu {
            message: "incomplete MFT coverage",
        })?;
        ensure!(
            mft.len() % geometry.file_record_size() as usize == 0,
            InvalidSnafu {
                message: "MFT data size is not record-aligned"
            }
        );
        Ok(Self {
            manifest,
            geometry,
            extents: known,
            image,
            mft,
        })
    }

    pub fn image(&self) -> &[u8] {
        &self.image
    }
    pub fn records(&self) -> impl ExactSizeIterator<Item = &[u8]> {
        self.mft
            .chunks_exact(self.geometry.file_record_size() as usize)
    }
    pub fn record(&self, number: u64) -> Option<&[u8]> {
        let start = usize::try_from(number)
            .ok()?
            .checked_mul(self.geometry.file_record_size() as usize)?;
        self.mft
            .get(start..start.checked_add(self.geometry.file_record_size() as usize)?)
    }
}

fn add_runs(
    known: &mut Vec<Extent>,
    descriptor: NonResident<'_>,
    geometry: &VolumeGeometry,
) -> Result<()> {
    for run in descriptor.runs() {
        let run = run.map_err(invalid)?;
        run.validate_geometry(geometry).map_err(invalid)?;
        ensure!(
            run.lcn().is_some(),
            InvalidSnafu {
                message: "sparse MFT or attribute-list stream"
            }
        );
        let extent = Extent {
            vcn: run.vcn(),
            lcn: run.lcn(),
            len: run.length(),
        };
        if !known.contains(&extent) {
            known.push(extent);
        }
    }
    Ok(())
}
fn physical(image: &[u8], start: u64, len: u64) -> Result<&[u8]> {
    let end = start.checked_add(len).context(InvalidSnafu {
        message: "physical range overflow",
    })?;
    let start = usize::try_from(start).map_err(invalid)?;
    let end = usize::try_from(end).map_err(invalid)?;
    image.get(start..end).context(InvalidSnafu {
        message: "physical range outside image",
    })
}
fn read_virtual(
    image: &[u8],
    geometry: &VolumeGeometry,
    extents: &[Extent],
    offset: u64,
    length: u64,
) -> Result<Option<Vec<u8>>> {
    ensure!(
        length <= image.len() as u64,
        InvalidSnafu {
            message: "virtual read exceeds image capacity"
        }
    );
    let mut result = Vec::new();
    let mut position = offset;
    let mut remaining = length;
    let cluster = u64::from(geometry.cluster_size());
    while remaining > 0 {
        let vcn = position / cluster;
        let Some(extent) = extents.iter().find(|extent| {
            extent.vcn <= vcn
                && extent
                    .vcn
                    .checked_add(extent.len)
                    .is_some_and(|end| vcn < end)
        }) else {
            return Ok(None);
        };
        let lcn = extent
            .lcn
            .context(InvalidSnafu {
                message: "sparse bootstrap extent",
            })?
            .checked_add(vcn - extent.vcn)
            .context(InvalidSnafu {
                message: "LCN overflow",
            })?;
        let range = geometry.cluster_byte_range(lcn, 1).map_err(invalid)?;
        let within = position % cluster;
        let count = remaining.min(cluster - within);
        result.extend_from_slice(physical(image, range.start + within, count)?);
        position = position.checked_add(count).context(InvalidSnafu {
            message: "virtual address overflow",
        })?;
        remaining -= count;
    }
    Ok(Some(result))
}
