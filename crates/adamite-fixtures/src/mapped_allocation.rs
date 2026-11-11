//! Independent mapped-run allocation totals for reconstructed fixture streams.

use crate::fixture::{AddressSnafu, AttributeSnafu, InvalidSnafu, RunlistSnafu};
use crate::{FixtureError, ReferenceMft};
use adamite_ntfs::{
    boot::VolumeGeometry,
    record::{FileRecord, RecordClass},
};
use snafu::{OptionExt, ResultExt, ensure};
use std::collections::{BTreeMap, BTreeSet};

/// Mapped bytes per non-resident stream, computed independently of manifest sizes and core.
///
/// Streams are identified by base-record ordinal, raw attribute type and exact UTF-16 name units. Every live extension contributes to its referenced base, after checking that the base is live and its sequence matches. All segments contribute their LCN-bearing run lengths, including LCN zero; holes contribute no clusters. Totals include mapped preallocation beyond logical length and use the reference geometry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MappedAllocation {
    streams: BTreeMap<(u64, u32), BTreeMap<Vec<u16>, u64>>,
}

impl MappedAllocation {
    /// Counts mapped clusters across all live non-resident attributes in the reference MFT.
    ///
    /// Unused and deleted records are skipped. Segment grouping uses record headers.
    ///
    /// # Errors
    ///
    /// Returns [`FixtureError`] for corrupt or BAAD records, invalid attributes or runlists, physical runs outside the volume, missing or stale base references, or overflow while summing clusters or converting them to bytes.
    pub fn from_reference(reference: &ReferenceMft) -> Result<Self, FixtureError> {
        let geometry = reference.geometry();
        let mut result = Self {
            streams: BTreeMap::new(),
        };
        let mut bases = BTreeMap::new();
        let mut extensions = BTreeSet::new();
        for (ordinal, raw) in reference.records().enumerate() {
            let mut raw = raw.to_vec();
            let record = match RecordClass::classify(&mut raw) {
                RecordClass::Base(record) | RecordClass::Extension(record) => record,
                RecordClass::Unused | RecordClass::NotInUse(_) => continue,
                RecordClass::Corrupt(error) => return Err(FixtureError::from_record(error)),
                _ => {
                    return Err(FixtureError::Invalid {
                        message: "invalid live reference record",
                    });
                }
            };
            let base = record.header().base_reference();
            let base_record = if base.is_null() {
                let ordinal = u64::try_from(ordinal).context(AddressSnafu)?;
                bases.insert(ordinal, record.header().sequence());
                ordinal
            } else {
                extensions.insert((base.record(), base.sequence()));
                base.record()
            };
            result.add_record(base_record, &record, &geometry)?;
        }
        for (record, sequence) in extensions {
            ensure!(
                bases.get(&record) == Some(&sequence),
                InvalidSnafu {
                    message: "allocation extension base missing or sequence mismatch"
                }
            );
        }
        for names in result.streams.values_mut() {
            for clusters in names.values_mut() {
                *clusters = clusters
                    .checked_mul(u64::from(geometry.cluster_size()))
                    .context(InvalidSnafu {
                        message: "mapped allocation byte count overflow",
                    })?;
            }
        }
        Ok(result)
    }

    /// Looks up mapped bytes for a base record, raw attribute type and exact UTF-16 name.
    ///
    /// Returns `Some(0)` for a non-resident stream containing only holes or no runs. Returns `None` for absent streams and resident attributes. Names are compared as raw units, preserving case, embedded NULs and unpaired surrogates.
    #[must_use]
    pub fn stream_bytes(&self, base_record: u64, attribute_type: u32, name: &[u16]) -> Option<u64> {
        self.streams
            .get(&(base_record, attribute_type))?
            .get(name)
            .copied()
    }

    fn add_record(
        &mut self,
        base_record: u64,
        record: &FileRecord<'_>,
        geometry: &VolumeGeometry,
    ) -> Result<(), FixtureError> {
        for attribute in record.attributes() {
            let attribute = attribute.context(AttributeSnafu)?;
            let Some(descriptor) = attribute.non_resident() else {
                continue;
            };
            let clusters = self
                .streams
                .entry((base_record, attribute.attribute_type().raw()))
                .or_default()
                .entry(attribute.name().units().collect())
                .or_default();
            for run in descriptor.runs() {
                let run = run.context(RunlistSnafu)?;
                run.validate_geometry(geometry).context(RunlistSnafu)?;
                if run.lcn().is_some() {
                    *clusters = clusters.checked_add(run.length()).context(InvalidSnafu {
                        message: "mapped allocation cluster count overflow",
                    })?;
                }
            }
        }
        Ok(())
    }
}
