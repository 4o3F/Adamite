//! Owned NTFS contributions retained until extension merge.

use crate::{error::ScanError, util::VecExt};
use std::num::NonZeroU32;

use adamite_ntfs::{
    attribute::{AttributeFlags, AttributeType, non_resident::StreamSizes},
    record::{RecordFlags, reference::FileRef},
    value::standard_information::StandardInformation,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecordKind {
    Unused,
    Baad,
    Deleted,
    Base,
    Extension,
    Corrupt,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct RecordSlot {
    pub classification: RecordKind,
    base: Option<NonZeroU32>,
}

/// Ordinal slots index only successfully decoded base contributions.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct RecordSlots {
    slots: Vec<RecordSlot>,
    bases: Vec<Option<RecordContribution>>,
}

impl RecordSlots {
    pub(crate) fn new(count: usize) -> Result<Self, ScanError> {
        let mut slots = Vec::new();
        slots.try_reserve_exact(count)?;
        Ok(Self {
            slots,
            bases: Vec::new(),
        })
    }

    pub(crate) fn len(&self) -> usize {
        self.slots.len()
    }

    pub(crate) fn get(&self, ordinal: usize) -> Option<&RecordSlot> {
        self.slots.get(ordinal)
    }

    pub(crate) fn push(
        &mut self,
        classification: RecordKind,
        base: Option<RecordContribution>,
    ) -> Result<(), ScanError> {
        self.slots.try_reserve(1)?;
        let base = if let Some(base) = base {
            let index = self
                .bases
                .len()
                .checked_add(1)
                .and_then(|index| u32::try_from(index).ok())
                .and_then(NonZeroU32::new)
                .ok_or_else(|| ScanError::overflow("base contribution index"))?;
            self.bases.try_push(Some(base))?;
            Some(index)
        } else {
            None
        };
        self.slots.push(RecordSlot {
            classification,
            base,
        });
        Ok(())
    }

    fn base_index(&self, ordinal: usize) -> Option<usize> {
        let slot = self.slots.get(ordinal)?;
        if slot.classification != RecordKind::Base {
            return None;
        }
        usize::try_from(slot.base?.get()).ok()?.checked_sub(1)
    }

    pub(crate) fn base(&self, ordinal: usize) -> Option<&RecordContribution> {
        self.bases.get(self.base_index(ordinal)?)?.as_ref()
    }

    pub(crate) fn base_mut(&mut self, ordinal: usize) -> Option<&mut RecordContribution> {
        let index = self.base_index(ordinal)?;
        self.bases.get_mut(index)?.as_mut()
    }

    pub(crate) fn take_base(&mut self, ordinal: usize) -> Option<RecordContribution> {
        let index = self.base_index(ordinal)?;
        self.bases.get_mut(index)?.take()
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct NameAttribute {
    pub parent: FileRef,
    pub namespace: u8,
    pub name: Vec<u16>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct StandardAttribute {
    pub value: StandardInformation,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ReparseAttribute {
    pub tag: u32,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum StreamStorage {
    Resident {
        size: u64,
    },
    NonResident {
        lowest_vcn: u64,
        highest_vcn: i64,
        compression_unit: u8,
        sizes: Option<StreamSizes>,
        has_runs: bool,
        mapped_clusters: u64,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct StreamSegment {
    pub source: FileRef,
    pub attribute_type: AttributeType,
    pub name: Vec<u16>,
    pub attribute_id: u16,
    pub flags: AttributeFlags,
    pub storage: StreamStorage,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct RecordContribution {
    pub reference: FileRef,
    pub hard_links: u16,
    pub flags: RecordFlags,
    pub conflicting_standard_information: bool,
    pub names: Vec<NameAttribute>,
    pub standard_information: Option<StandardAttribute>,
    pub streams: Vec<StreamSegment>,
    pub reparse_tags: Vec<ReparseAttribute>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Patch {
    pub base: FileRef,
    pub source: FileRef,
    // Stream attribute ids are retained for segment identity.
    pub contribution: RecordContribution,
}

impl RecordContribution {
    /// Preserve the first value and defer conflicting-value rejection until merge.
    pub(crate) fn add_standard(&mut self, standard: StandardAttribute) {
        if let Some(first) = &self.standard_information {
            self.conflicting_standard_information |= first != &standard;
        } else {
            self.standard_information = Some(standard);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{RecordContribution, RecordKind, RecordSlot, RecordSlots};
    use adamite_ntfs::record::{RecordFlags, reference::FileRef};
    use std::{error::Error, mem::size_of};

    fn contribution(record: u64) -> Result<RecordContribution, Box<dyn Error>> {
        Ok(RecordContribution {
            reference: FileRef::new(record, 7)?,
            hard_links: 1,
            flags: RecordFlags::from_raw(1),
            conflicting_standard_information: false,
            names: Vec::new(),
            standard_information: None,
            streams: Vec::new(),
            reparse_tags: Vec::new(),
        })
    }

    #[test]
    fn vacant_slots_use_no_contribution_storage_and_reserve_exactly() -> Result<(), Box<dyn Error>>
    {
        let mut slots = RecordSlots::new(257)?;
        assert_eq!(slots.slots.capacity(), 257);
        assert!(size_of::<RecordSlot>() <= 8);
        for ordinal in 0..257 {
            slots.push(RecordKind::Unused, None)?;
            assert!(slots.base(ordinal).is_none());
            assert!(slots.base_mut(ordinal).is_none());
            assert!(slots.take_base(ordinal).is_none());
        }
        assert_eq!(slots.slots.capacity(), 257);
        assert_eq!(slots.bases.capacity(), 0);
        assert!(slots.base(257).is_none());
        Ok(())
    }

    #[test]
    fn dense_bases_keep_ordinal_identity_across_holes_and_consumption() -> Result<(), Box<dyn Error>>
    {
        let mut slots = RecordSlots::new(7)?;
        for kind in [
            RecordKind::Unused,
            RecordKind::Deleted,
            RecordKind::Baad,
            RecordKind::Corrupt,
            RecordKind::Extension,
        ] {
            slots.push(kind, None)?;
        }
        slots.push(RecordKind::Base, Some(contribution(5)?))?;
        slots.push(RecordKind::Base, Some(contribution(6)?))?;
        assert_eq!(slots.bases.len(), 2);
        slots.base_mut(6).ok_or("base 6")?.hard_links = 3;
        assert_eq!(slots.take_base(5).ok_or("base 5")?.reference.record(), 5);
        assert!(slots.take_base(5).is_none());
        let base = slots.take_base(6).ok_or("base 6")?;
        assert_eq!((base.reference.record(), base.hard_links), (6, 3));
        assert!(slots.base(6).is_none());
        Ok(())
    }
}
