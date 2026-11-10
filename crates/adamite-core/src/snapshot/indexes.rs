//! Read-only summaries of primary files grouped by their filename extension.

/// Primary-file count and self sizes for one ASCII-lowercased filename extension.
///
/// The extension excludes the dot; an empty string groups names without a usable suffix.
/// Logical bytes exclude ADS, and allocation includes every non-resident stream.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct ExtensionStat {
    pub(crate) extension: String,
    pub(crate) count: u64,
    pub(crate) total_size: u64,
    pub(crate) total_alloc: u64,
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
}
