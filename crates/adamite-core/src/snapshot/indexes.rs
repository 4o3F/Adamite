//! Read-only summaries of primary files grouped by their filename extension.

use super::{FileKind, SnapshotError, rules::Extension};
use std::{cmp::Ordering, collections::BinaryHeap, vec::IntoIter};

/// Primary-file count and self sizes for one ASCII-lowercased filename extension.
///
/// The extension excludes the dot; an empty string groups names without a usable suffix.
/// Logical bytes exclude ADS, and allocation includes every non-resident stream.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct ExtensionStat {
    pub(super) extension: String,
    pub(super) count: u64,
    pub(super) total_size: u64,
    pub(super) total_alloc: u64,
}

impl ExtensionStat {
    /// Borrows the suffix with ASCII letters lowercased and no leading dot.
    #[must_use]
    pub fn extension(&self) -> &str {
        &self.extension
    }
    /// Returns the number of primary files in this group.
    #[must_use]
    pub const fn count(&self) -> u64 {
        self.count
    }
    /// Returns unnamed DATA logical bytes, excluding ADS.
    #[must_use]
    pub const fn total_size(&self) -> u64 {
        self.total_size
    }
    /// Returns allocation across all non-resident streams of these files.
    #[must_use]
    pub const fn total_alloc(&self) -> u64 {
        self.total_alloc
    }
    pub(super) fn compare(&self, other: &Self) -> Ordering {
        other
            .total_alloc
            .cmp(&self.total_alloc)
            .then_with(|| self.extension.cmp(&other.extension))
    }

    pub(super) fn try_clone(&self) -> Result<Self, SnapshotError> {
        let mut extension = String::new();
        extension.try_reserve_exact(self.extension.len())?;
        extension.push_str(&self.extension);
        Ok(Self {
            extension,
            count: self.count,
            total_size: self.total_size,
            total_alloc: self.total_alloc,
        })
    }
}

/// Primary-file count and self sizes for one file category.
///
/// Logical bytes exclude ADS, and allocation includes every non-resident stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KindStat {
    pub(super) kind: FileKind,
    pub(super) count: u64,
    pub(super) total_size: u64,
    pub(super) total_alloc: u64,
}

impl KindStat {
    /// Returns the category inferred from filename extensions.
    #[must_use]
    pub const fn kind(&self) -> FileKind {
        self.kind
    }
    /// Returns the number of primary files in this category.
    #[must_use]
    pub const fn count(&self) -> u64 {
        self.count
    }
    /// Returns unnamed DATA logical bytes, excluding ADS.
    #[must_use]
    pub const fn total_size(&self) -> u64 {
        self.total_size
    }
    /// Returns allocation across all non-resident streams of these files.
    #[must_use]
    pub const fn total_alloc(&self) -> u64 {
        self.total_alloc
    }

    pub(super) fn add(&mut self, group: &ExtensionStat) -> Result<(), SnapshotError> {
        let count = self
            .count
            .checked_add(group.count)
            .ok_or_else(|| SnapshotError::overflow("kind count"))?;
        let size = self
            .total_size
            .checked_add(group.total_size)
            .ok_or_else(|| SnapshotError::overflow("kind logical bytes"))?;
        let alloc = self
            .total_alloc
            .checked_add(group.total_alloc)
            .ok_or_else(|| SnapshotError::overflow("kind allocation"))?;
        self.count = count;
        self.total_size = size;
        self.total_alloc = alloc;
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq)]
struct RankedExtension(ExtensionStat);

impl Ord for RankedExtension {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .0
            .compare(&self.0)
            .then_with(|| self.0.count.cmp(&other.0.count))
            .then_with(|| self.0.total_size.cmp(&other.0.total_size))
    }
}

impl PartialOrd for RankedExtension {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Accumulates by folded suffix, then emits allocation/name order in bounded steps.
#[derive(Debug, Default)]
pub(super) struct ExtensionGroups {
    groups: Vec<ExtensionStat>,
    pub(super) pending: IntoIter<ExtensionStat>,
    heap: BinaryHeap<RankedExtension>,
}

impl ExtensionGroups {
    pub(super) fn len(&self) -> usize {
        self.groups.len()
    }

    pub(super) fn add(
        &mut self,
        suffix: Extension<'_>,
        size: u64,
        alloc: u64,
    ) -> Result<(), SnapshotError> {
        match self
            .groups
            .binary_search_by(|group| suffix.compare_folded(&group.extension))
        {
            Ok(position) => {
                let group = self
                    .groups
                    .get_mut(position)
                    .ok_or_else(|| SnapshotError::invalid("missing extension group"))?;
                let count = group
                    .count
                    .checked_add(1)
                    .ok_or_else(|| SnapshotError::overflow("extension count"))?;
                let size = group
                    .total_size
                    .checked_add(size)
                    .ok_or_else(|| SnapshotError::overflow("extension logical bytes"))?;
                let alloc = group
                    .total_alloc
                    .checked_add(alloc)
                    .ok_or_else(|| SnapshotError::overflow("extension allocation"))?;
                group.count = count;
                group.total_size = size;
                group.total_alloc = alloc;
            }
            Err(position) => {
                let extension = suffix.folded()?;
                self.groups.try_reserve(1)?;
                self.groups.insert(
                    position,
                    ExtensionStat {
                        extension,
                        count: 1,
                        total_size: size,
                        total_alloc: alloc,
                    },
                );
            }
        }
        Ok(())
    }

    pub(super) fn prepare_ordering(&mut self) -> Result<(), SnapshotError> {
        self.heap.try_reserve_exact(self.groups.len())?;
        self.pending = std::mem::take(&mut self.groups).into_iter();
        Ok(())
    }

    pub(super) fn build_next(&mut self) -> bool {
        if let Some(group) = self.pending.next() {
            self.heap.push(RankedExtension(group));
            true
        } else {
            self.pending = Vec::new().into_iter();
            false
        }
    }

    pub(super) fn pop_ordered(&mut self) -> Option<ExtensionStat> {
        self.heap.pop().map(|group| group.0)
    }

    pub(super) fn into_sorted(mut self) -> Result<Vec<ExtensionStat>, SnapshotError> {
        let mut result = Vec::new();
        result.try_reserve_exact(self.len())?;
        self.prepare_ordering()?;
        while self.build_next() {}
        while let Some(group) = self.pop_ordered() {
            result.push(group);
        }
        Ok(result)
    }
}
