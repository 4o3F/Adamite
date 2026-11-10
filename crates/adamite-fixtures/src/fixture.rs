//! Image loading and independent MFT reconstruction for committed NTFS fixtures.

use crate::{Extent, Manifest};
use adamite_ntfs::fixup::FixupError;
use adamite_ntfs::{
    AttributeError, AttributeList, AttributeListError, AttributeType, FileRecord, FileRef,
    FileRefError, GeometryError, NonResident, RecordError, RunlistError, VolumeGeometry, fixup,
    parse_boot_sector,
};
use ruzstd::decoding::{StreamingDecoder, errors::FrameDecoderError};
use snafu::{OptionExt, ResultExt, Snafu, ensure};
use std::{
    collections::BTreeSet,
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
};

/// Image loading, manifest validation or reference MFT reconstruction failed.
#[derive(Debug, Snafu)]
#[snafu(visibility(pub(crate)))]
#[non_exhaustive]
pub enum FixtureError {
    /// Fixture I/O failed.
    #[snafu(display("fixture I/O failed for {}: {source}", path.display()))]
    Io {
        /// File being read.
        path: PathBuf,
        /// The underlying error.
        source: io::Error,
    },
    /// Invalid fixture manifest.
    #[snafu(display("invalid fixture manifest {}: {source}", path.display()))]
    Json {
        /// Manifest being decoded.
        path: PathBuf,
        /// The underlying error.
        source: serde_json::Error,
    },
    /// Invalid zstd frame.
    #[snafu(display("invalid zstd frame in {}: {source}", path.display()))]
    Decode {
        /// Compressed image being decoded.
        path: PathBuf,
        /// The underlying error.
        source: FrameDecoderError,
    },
    /// Invalid volume geometry.
    #[snafu(display("invalid volume geometry: {source}"))]
    Geometry {
        /// The underlying error.
        source: GeometryError,
    },
    /// Invalid record fixup.
    #[snafu(display("invalid record fixup: {source}"))]
    Fixup {
        /// The underlying error.
        source: FixupError,
    },
    /// Invalid FILE record.
    #[snafu(display("invalid FILE record: {source}"))]
    Record {
        /// The underlying error.
        source: RecordError,
    },
    /// Invalid attribute.
    #[snafu(display("invalid attribute: {source}"))]
    Attribute {
        /// The underlying error.
        source: AttributeError,
    },
    /// Invalid attribute list.
    #[snafu(display("invalid attribute list: {source}"))]
    AttributeList {
        /// The underlying error.
        source: AttributeListError,
    },
    /// Invalid runlist.
    #[snafu(display("invalid runlist: {source}"))]
    Runlist {
        /// The underlying error.
        source: RunlistError,
    },
    /// Invalid file reference.
    #[snafu(display("invalid file reference: {source}"))]
    FileRef {
        /// The underlying error.
        source: FileRefError,
    },
    /// Unrepresentable byte offset.
    #[snafu(display("unrepresentable byte offset: {source}"))]
    Address {
        /// The underlying error.
        source: std::num::TryFromIntError,
    },
    /// Image or manifest values violate a fixture invariant.
    #[snafu(display("invalid fixture: {message}"))]
    Invalid {
        /// The invariant that failed.
        message: &'static str,
    },
}
type Result<T, E = FixtureError> = std::result::Result<T, E>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Request {
    record: u64,
    sequence: u16,
    id: u16,
    lowest: u64,
}

/// A decompressed image and independently generated manifest, without MFT reconstruction.
#[derive(Debug)]
pub struct Fixture {
    manifest: Manifest,
    image: Vec<u8>,
}

/// A validated MFT reconstructed independently from the image's own mapping pairs.
#[derive(Debug)]
pub struct ReferenceMft {
    geometry: VolumeGeometry,
    extents: Vec<Extent>,
    mft: Vec<u8>,
}

impl Fixture {
    /// Loads a sibling zstd image and checks its length, frame checksum and manifest.
    ///
    /// # Errors
    ///
    /// Returns an error for inaccessible files, invalid JSON, unsafe image names,
    /// decompression failures, checksum mismatches or invalid schema/length values.
    pub fn load(manifest_path: &Path) -> Result<Self> {
        let manifest: Manifest =
            serde_json::from_slice(&fs::read(manifest_path).context(IoSnafu {
                path: manifest_path,
            })?)
            .context(JsonSnafu {
                path: manifest_path,
            })?;
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
        let image_path = directory.join(&manifest.image);
        let source = fs::File::open(&image_path).context(IoSnafu { path: &image_path })?;
        let mut decoder =
            StreamingDecoder::new(source).context(DecodeSnafu { path: &image_path })?;
        let mut image = Vec::new();
        let limit = manifest.image_len.checked_add(1).context(InvalidSnafu {
            message: "image length overflow",
        })?;
        decoder
            .by_ref()
            .take(limit)
            .read_to_end(&mut image)
            .context(IoSnafu { path: &image_path })?;
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

    /// Validates schema and length for an already decompressed image.
    ///
    /// # Errors
    ///
    /// Returns an error for unsupported schemas, missing v3 measurements or a length mismatch.
    pub fn from_image(image: Vec<u8>, manifest: Manifest) -> Result<Self> {
        ensure!(
            u64::try_from(image.len()).ok() == Some(manifest.image_len),
            InvalidSnafu {
                message: "schema version or image length mismatch"
            }
        );
        manifest
            .validate()
            .map_err(|message| FixtureError::Invalid { message })?;
        Ok(Self { manifest, image })
    }

    /// Borrows the complete decompressed image, without interpreting its MFT.
    #[must_use]
    pub fn image(&self) -> &[u8] {
        &self.image
    }

    /// Borrows the independently generated manifest.
    #[must_use]
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }
}

impl ReferenceMft {
    /// Reconstructs the MFT and checks its geometry and extents against the manifest.
    ///
    /// # Errors
    ///
    /// Returns a parser or fixture error for invalid geometry, records, mapping pairs,
    /// unreachable extension records or disagreement with the manifest.
    pub fn from_fixture(fixture: &Fixture) -> Result<Self> {
        let image = fixture.image();
        let manifest = fixture.manifest();
        let geometry = parse_boot_sector(image).context(GeometrySnafu)?;
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
        let mut raw = physical(image, start, u64::from(geometry.file_record_size()))?.to_vec();
        let record =
            FileRecord::parse(fixup::apply(&mut raw).context(FixupSnafu)?).context(RecordSnafu)?;
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
            let attribute = attribute.context(AttributeSnafu)?;
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
                        size <= 262_144,
                        InvalidSnafu {
                            message: "attribute list exceeds 256 KiB"
                        }
                    );
                    let mut runs = Vec::new();
                    add_runs(&mut runs, descriptor, &geometry)?;
                    read_virtual(image, &geometry, &runs, 0, size)?.context(InvalidSnafu {
                        message: "incomplete attribute-list extents",
                    })?
                };
                for entry in AttributeList::new(&value).context(AttributeListSnafu)? {
                    let entry = entry.context(AttributeListSnafu)?;
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
                    image,
                    &geometry,
                    &known,
                    offset,
                    u64::from(geometry.file_record_size()),
                )?
                else {
                    pending.push(request);
                    continue;
                };
                let record = FileRecord::parse(fixup::apply(&mut raw).context(FixupSnafu)?)
                    .context(RecordSnafu)?;
                ensure!(
                    record.header().sequence() == request.sequence
                        && record.header().base_reference()
                            == FileRef::new(0, sequence).context(FileRefSnafu)?,
                    InvalidSnafu {
                        message: "MFT extension reference or sequence mismatch"
                    }
                );
                let mut found = false;
                for attribute in record.attributes() {
                    let attribute = attribute.context(AttributeSnafu)?;
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
        let mft = read_virtual(image, &geometry, &known, 0, data_size)?.context(InvalidSnafu {
            message: "incomplete MFT coverage",
        })?;
        ensure!(
            mft.len() % geometry.file_record_size() as usize == 0,
            InvalidSnafu {
                message: "MFT data size is not record-aligned"
            }
        );
        Ok(Self {
            geometry,
            extents: known,
            mft,
        })
    }

    /// Returns the validated volume geometry.
    #[must_use]
    pub fn geometry(&self) -> VolumeGeometry {
        self.geometry
    }

    /// Borrows the discovered MFT extents in VCN order.
    #[must_use]
    pub fn extents(&self) -> &[Extent] {
        &self.extents
    }

    /// Iterates complete raw records before update-sequence fixup.
    pub fn records(&self) -> impl ExactSizeIterator<Item = &[u8]> {
        self.mft
            .chunks_exact(self.geometry.file_record_size() as usize)
    }
    /// Borrows a raw record by its virtual MFT ordinal.
    #[must_use]
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
        let run = run.context(RunlistSnafu)?;
        run.validate_geometry(geometry).context(RunlistSnafu)?;
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
    let start = usize::try_from(start).context(AddressSnafu)?;
    let end = usize::try_from(end).context(AddressSnafu)?;
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
        let range = geometry.cluster_byte_range(lcn, 1).context(GeometrySnafu)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        error::Error,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    const MANIFEST: &str = include_str!("../../../tests/fixtures/ntfs/basic-4k.json");

    #[test]
    fn raw_image_loading_does_not_require_a_valid_mft() {
        let mut manifest: Manifest = serde_json::from_str(MANIFEST).unwrap();
        let schema_version = manifest.schema_version;
        manifest.image_len = 512;
        let fixture = Fixture::from_image(vec![0; 512], manifest).unwrap();
        assert_eq!(fixture.image(), &[0; 512]);
        assert_eq!(fixture.manifest().schema_version, schema_version);

        let error = ReferenceMft::from_fixture(&fixture).unwrap_err();
        assert!(matches!(error, FixtureError::Geometry { .. }));
        assert!(error.source().is_some());
    }

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            loop {
                let path = std::env::temp_dir().join(format!(
                    "adamite-fixtures-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
                match fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                    Err(error) => panic!("cannot create fixture test directory: {error}"),
                }
            }
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn loading_errors_identify_the_manifest_or_image_and_keep_the_io_source() {
        let directory = TestDirectory::new();
        let manifest_path = directory.0.join("basic-4k.json");
        let error = Fixture::load(&manifest_path).unwrap_err();
        assert!(matches!(
            &error,
            FixtureError::Io { path, source }
                if path == &manifest_path && source.kind() == io::ErrorKind::NotFound
        ));
        assert!(error.source().unwrap().is::<io::Error>());
        assert!(
            error
                .to_string()
                .contains(&manifest_path.display().to_string())
        );

        fs::write(&manifest_path, MANIFEST).unwrap();
        let manifest: Manifest = serde_json::from_str(MANIFEST).unwrap();
        let image_path = directory.0.join(manifest.image);
        let error = Fixture::load(&manifest_path).unwrap_err();
        assert!(matches!(
            &error,
            FixtureError::Io { path, source }
                if path == &image_path && source.kind() == io::ErrorKind::NotFound
        ));
        assert!(error.source().unwrap().is::<io::Error>());
        assert!(
            error
                .to_string()
                .contains(&image_path.display().to_string())
        );
    }

    #[test]
    fn decode_errors_identify_the_file_and_keep_the_decoder_source() {
        let directory = TestDirectory::new();
        let manifest_path = directory.0.join("basic-4k.json");
        fs::write(&manifest_path, b"{").unwrap();
        let error = Fixture::load(&manifest_path).unwrap_err();
        assert!(matches!(
            &error,
            FixtureError::Json { path, .. } if path == &manifest_path
        ));
        assert!(error.source().unwrap().is::<serde_json::Error>());
        assert!(
            error
                .to_string()
                .contains(&manifest_path.display().to_string())
        );

        fs::write(&manifest_path, MANIFEST).unwrap();
        let manifest: Manifest = serde_json::from_str(MANIFEST).unwrap();
        let image_path = directory.0.join(manifest.image);
        fs::write(&image_path, b"invalid zstd frame").unwrap();
        let error = Fixture::load(&manifest_path).unwrap_err();
        assert!(matches!(
            &error,
            FixtureError::Decode { path, .. } if path == &image_path
        ));
        assert!(error.source().unwrap().is::<FrameDecoderError>());
        assert!(
            error
                .to_string()
                .contains(&image_path.display().to_string())
        );
    }

    #[test]
    fn compressed_image_checksum_is_checked_during_loading() {
        let directory = TestDirectory::new();
        let manifest: Manifest = serde_json::from_str(MANIFEST).unwrap();
        let mut image = include_bytes!("../../../tests/fixtures/ntfs/basic-4k.img.zst").to_vec();
        *image.last_mut().unwrap() ^= 1;
        fs::write(directory.0.join(&manifest.image), image).unwrap();
        let manifest_path = directory.0.join("basic-4k.json");
        fs::write(&manifest_path, MANIFEST).unwrap();

        let error = Fixture::load(&manifest_path).unwrap_err();
        assert!(error.to_string().contains("checksum"), "{error}");
    }
}
