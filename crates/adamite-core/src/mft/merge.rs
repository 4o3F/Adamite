//! Transactional patch application and per-stream accounting before generic linking.

use crate::{
    error::ScanError,
    graph::entry::{EntryName, EntryRecord},
    mft::model::{Patch, RecordContribution, RecordSlots, StreamSegment, StreamStorage},
    scan::ScanStats,
    snapshot::NodeFlags,
    util::{CounterExt, VecExt},
};
use adamite_ntfs::{
    attribute::{AttributeFlags, AttributeType},
    record::{
        RecordFlags,
        system::{BAD_CLUS, ROOT},
    },
    value::standard_information::FileAttributes,
};
use std::{cmp::Ordering, vec::IntoIter};

/// Maximum patches or base records processed before yielding to the caller.
const ITEMS_PER_STEP: usize = 256;
/// The named DATA stream used by $BadClus; its logical size describes volume coverage.
const BAD_STREAM_NAME: &[u16] = &[36, 66, 97, 100];

#[derive(Debug)]
enum Stage {
    Patches,
    Entries,
}

#[derive(Debug)]
pub(crate) struct Merge {
    slots: RecordSlots,
    patches: IntoIter<Patch>,
    bases: IntoIter<RecordContribution>,
    cluster_size: u32,
    stage: Stage,
    root_emitted: bool,
    pub entries: Vec<EntryRecord>,
}

impl Merge {
    fn reserve_contribution<T>(target: &mut Vec<T>, additional: usize) -> Result<(), ScanError> {
        target
            .len()
            .checked_add(additional)
            .ok_or_else(|| ScanError::overflow("merged contribution length"))?;
        VecExt::try_reserve_exact(target, additional)?;
        Ok(())
    }

    pub(crate) fn new(slots: RecordSlots, patches: Vec<Patch>, cluster_size: u32) -> Self {
        Self {
            slots,
            patches: patches.into_iter(),
            bases: Vec::new().into_iter(),
            cluster_size,
            stage: Stage::Patches,
            root_emitted: false,
            entries: Vec::new(),
        }
    }

    /// Returns true only after every patch and base has been considered.
    pub(crate) fn step(&mut self, stats: &mut ScanStats) -> Result<bool, ScanError> {
        for _ in 0..ITEMS_PER_STEP {
            match self.stage {
                Stage::Patches => {
                    let Some(patch) = self.patches.next() else {
                        self.patches = Vec::new().into_iter();
                        self.bases = std::mem::take(&mut self.slots).into_bases();
                        self.stage = Stage::Entries;
                        return Ok(false);
                    };
                    self.apply_patch(patch, stats)?;
                }
                Stage::Entries => {
                    let Some(base) = self.bases.next() else {
                        self.bases = Vec::new().into_iter();
                        if !self.root_emitted {
                            return Err(ScanError::RequiredRecord { record: ROOT });
                        }
                        return Ok(true);
                    };
                    let is_root = base.reference.record() == ROOT;
                    if let Some(entry) = base.into_entry(self.cluster_size, stats)? {
                        self.entries.try_push(entry)?;
                        self.root_emitted |= is_root;
                    } else if is_root {
                        return Err(ScanError::RequiredRecord { record: ROOT });
                    }
                }
            }
        }
        Ok(false)
    }

    fn apply_patch(&mut self, patch: Patch, stats: &mut ScanStats) -> Result<(), ScanError> {
        let index = usize::try_from(patch.base.record()).ok();
        let valid = index
            .and_then(|index| self.slots.base(index))
            .is_some_and(|base| {
                base.reference == patch.base && base.flags.contains(RecordFlags::IN_USE)
            })
            && patch.source == patch.contribution.reference
            && patch
                .contribution
                .streams
                .iter()
                .all(|segment| segment.source == patch.source);
        if !valid {
            stats.rejected_extensions.try_bump()?;
            stats.invalid_file_refs.try_bump()?;
            return Ok(());
        }
        let base = index
            .and_then(|index| self.slots.base_mut(index))
            .ok_or(ScanError::Stream {
                reason: "validated patch base disappeared",
            })?;
        let mut added = patch.contribution;
        // Reserve every destination before changing any logical contents.
        Self::reserve_contribution(&mut base.names, added.names.len())?;
        Self::reserve_contribution(&mut base.streams, added.streams.len())?;
        Self::reserve_contribution(&mut base.reparse_tags, added.reparse_tags.len())?;
        base.names.append(&mut added.names);
        if let Some(standard) = added.standard_information {
            base.add_standard(standard);
        }
        base.conflicting_standard_information |= added.conflicting_standard_information;
        base.streams.append(&mut added.streams);
        base.reparse_tags.append(&mut added.reparse_tags);
        Ok(())
    }
}

#[derive(Debug)]
enum StreamError {
    Invalid,
    Fatal(ScanError),
}

impl From<ScanError> for StreamError {
    fn from(error: ScanError) -> Self {
        Self::Fatal(error)
    }
}

#[derive(Debug)]
struct StreamTotals {
    logical: u64,
    alloc: u64,
    sparse: bool,
    compressed: bool,
}

impl StreamSegment {
    fn compare(&self, right: &Self) -> Ordering {
        (
            self.attribute_type.raw(),
            &self.name,
            self.lowest_vcn(),
            self.source.record(),
            self.source.sequence(),
            self.attribute_id,
        )
            .cmp(&(
                right.attribute_type.raw(),
                &right.name,
                right.lowest_vcn(),
                right.source.record(),
                right.source.sequence(),
                right.attribute_id,
            ))
    }

    fn lowest_vcn(&self) -> u64 {
        match self.storage {
            StreamStorage::Resident { .. } => 0,
            StreamStorage::NonResident { lowest_vcn, .. } => lowest_vcn,
        }
    }
}

impl StreamTotals {
    fn from_segments(
        segments: &[StreamSegment],
        cluster_size: u32,
    ) -> Result<StreamTotals, StreamError> {
        let first = segments.first().ok_or(StreamError::Invalid)?;
        let sparse = first.flags.contains(AttributeFlags::SPARSE);
        let compressed = first.flags.compression_format() != 0;
        if let StreamStorage::Resident { size } = first.storage {
            if segments.len() != 1 {
                return Err(StreamError::Invalid);
            }
            return Ok(StreamTotals {
                logical: size,
                alloc: 0,
                sparse,
                compressed,
            });
        }
        let StreamStorage::NonResident {
            sizes: Some(sizes),
            compression_unit,
            ..
        } = first.storage
        else {
            return Err(StreamError::Invalid);
        };
        let mut expected = 0u64;
        let mut mapped_clusters = 0u64;
        for (index, segment) in segments.iter().enumerate() {
            let StreamStorage::NonResident {
                lowest_vcn,
                highest_vcn,
                sizes,
                compression_unit: unit,
                has_runs,
                mapped_clusters: mapped,
            } = segment.storage
            else {
                return Err(StreamError::Invalid);
            };
            if lowest_vcn != expected
                || segment.flags != first.flags
                || unit != compression_unit
                || (index == 0 && lowest_vcn != 0)
                || (index != 0 && sizes.is_some())
            {
                return Err(StreamError::Invalid);
            }
            if highest_vcn == -1 {
                if segments.len() != 1 || lowest_vcn != 0 || has_runs {
                    return Err(StreamError::Invalid);
                }
                expected = 0;
            } else {
                if !has_runs {
                    return Err(StreamError::Invalid);
                }
                expected = u64::try_from(highest_vcn)
                    .ok()
                    .and_then(|highest| highest.checked_add(1))
                    .ok_or(StreamError::Invalid)?;
                if expected <= lowest_vcn {
                    return Err(StreamError::Invalid);
                }
            }
            mapped_clusters = mapped_clusters
                .checked_add(mapped)
                .ok_or_else(|| ScanError::overflow("merged stream mapped clusters"))?;
        }
        let mapped_bytes = mapped_clusters
            .checked_mul(u64::from(cluster_size))
            .ok_or_else(|| ScanError::overflow("merged stream mapped bytes"))?;
        let coverage = expected
            .checked_mul(u64::from(cluster_size))
            .ok_or_else(|| ScanError::overflow("merged stream coverage"))?;
        if coverage < sizes.data() {
            return Err(StreamError::Invalid);
        }
        let alloc = if sparse || compressed {
            sizes.compressed().ok_or(StreamError::Invalid)?
        } else {
            mapped_bytes
        };
        Ok(StreamTotals {
            logical: sizes.data(),
            alloc,
            sparse,
            compressed,
        })
    }
}

impl RecordContribution {
    fn entry_flags(
        record_flags: RecordFlags,
        attributes: FileAttributes,
        reparse_tag: Option<u32>,
    ) -> NodeFlags {
        let mut flags = NodeFlags::default();
        if record_flags.contains(RecordFlags::DIRECTORY)
            || attributes.contains(FileAttributes::DIRECTORY)
        {
            flags.insert(NodeFlags::DIRECTORY);
        }
        if reparse_tag.is_some() || attributes.contains(FileAttributes::REPARSE_POINT) {
            flags.insert(NodeFlags::REPARSE);
        }
        if attributes.contains(FileAttributes::SPARSE_FILE) {
            flags.insert(NodeFlags::SPARSE);
        }
        if attributes.contains(FileAttributes::COMPRESSED) {
            flags.insert(NodeFlags::COMPRESSED);
        }
        if attributes.contains(FileAttributes::HIDDEN) {
            flags.insert(NodeFlags::HIDDEN);
        }
        if attributes.contains(FileAttributes::SYSTEM) {
            flags.insert(NodeFlags::SYSTEM);
        }
        flags
    }

    fn into_entry(
        mut self,
        cluster_size: u32,
        stats: &mut ScanStats,
    ) -> Result<Option<EntryRecord>, ScanError> {
        let standard = self
            .standard_information
            .as_ref()
            .map(|attribute| attribute.value);
        if self.conflicting_standard_information
            || self.reparse_tags.iter().any(|tag| {
                self.reparse_tags
                    .first()
                    .is_some_and(|first| first.tag != tag.tag)
            })
        {
            stats.corrupt_values.try_bump()?;
            stats.corrupt_records.try_bump()?;
            return Ok(None);
        }
        // Absent SI has zero attributes and time; a valid extension may supply it.
        let attrs = standard.map_or(0, |value| value.file_attributes().raw());
        let mtime = standard.map_or(0, |value| value.times().modified());
        let reparse_tag = self.reparse_tags.first().map(|attribute| attribute.tag);
        let mut flags = Self::entry_flags(self.flags, FileAttributes::from_raw(attrs), reparse_tag);
        self.streams.sort_unstable_by(StreamSegment::compare);
        let mut size = 0u64;
        let mut ads_size = 0u64;
        let mut alloc = 0u64;
        let mut rejected = false;
        let mut start = 0usize;
        while let Some(first) = self.streams.get(start) {
            let mut end = start
                .checked_add(1)
                .ok_or_else(|| ScanError::overflow("stream group cursor"))?;
            while self.streams.get(end).is_some_and(|segment| {
                segment.attribute_type == first.attribute_type && segment.name == first.name
            }) {
                end = end
                    .checked_add(1)
                    .ok_or_else(|| ScanError::overflow("stream group cursor"))?;
            }
            let group = self
                .streams
                .get(start..end)
                .ok_or_else(|| ScanError::overflow("stream group range"))?;
            match StreamTotals::from_segments(group, cluster_size) {
                Ok(total) => {
                    alloc = alloc
                        .checked_add(total.alloc)
                        .ok_or_else(|| ScanError::overflow("entry allocation"))?;
                    if total.sparse {
                        flags.insert(NodeFlags::SPARSE);
                    }
                    if total.compressed {
                        flags.insert(NodeFlags::COMPRESSED);
                    }
                    if first.attribute_type == AttributeType::DATA {
                        if first.name.is_empty() {
                            size = total.logical;
                        } else if !(self.reference.record() == BAD_CLUS
                            && first.name == BAD_STREAM_NAME)
                        {
                            ads_size = ads_size
                                .checked_add(total.logical)
                                .ok_or_else(|| ScanError::overflow("entry ADS size"))?;
                        }
                    }
                }
                Err(StreamError::Invalid) => {
                    stats.corrupt_streams.try_bump()?;
                    rejected = true;
                }
                Err(StreamError::Fatal(error)) => return Err(error),
            }
            start = end;
        }
        if rejected {
            stats.corrupt_records.try_bump()?;
            return Ok(None);
        }
        if self.reference.record() == ROOT && !flags.contains(NodeFlags::DIRECTORY) {
            return Err(ScanError::RequiredRecord { record: ROOT });
        }
        let mut names = Vec::new();
        names.try_reserve_exact(self.names.len())?;
        names.extend(self.names.into_iter().map(|name| EntryName {
            parent: name.parent,
            namespace: name.namespace,
            name: name.name,
        }));
        Ok(Some(EntryRecord {
            file_ref: self.reference,
            attrs,
            mtime,
            size,
            ads_size,
            alloc,
            names,
            nlink: self.hard_links,
            reparse_tag,
            flags,
        }))
    }
}

#[cfg(test)]
mod tests;
