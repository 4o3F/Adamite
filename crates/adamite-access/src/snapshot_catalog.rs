use adamite_io::snapshot_dir::{SnapshotDir, SnapshotDirError, SnapshotEntry};

/// Discovers saved snapshots and matches a drive to its single-letter filename stem.
#[derive(Debug)]
pub struct SnapshotCatalog {
    directory: SnapshotDir,
}

impl SnapshotCatalog {
    /// Uses a snapshot directory without creating or enumerating it.
    #[must_use]
    pub fn new(directory: SnapshotDir) -> Self {
        Self { directory }
    }

    /// Borrows the directory used for discovery and drive scan publication.
    #[must_use]
    pub fn directory(&self) -> &SnapshotDir {
        &self.directory
    }

    /// Refreshes the filename-sorted metadata listing, retaining unreadable entries as error rows.
    ///
    /// A missing directory is empty. Symlinks are followed; subdirectories are omitted.
    ///
    /// # Errors
    /// Returns an error when the directory itself cannot be opened.
    pub fn entries(&self) -> Result<Vec<SnapshotEntry>, SnapshotDirError> {
        self.directory.entries()
    }

    /// Refreshes discovery and returns the first snapshot claiming this ASCII drive letter.
    ///
    /// Matching ignores ASCII case and requires a single-letter stem, even for error rows.
    ///
    /// # Errors
    /// Returns an error when the directory itself cannot be opened.
    pub fn for_drive(&self, letter: char) -> Result<Option<SnapshotEntry>, SnapshotDirError> {
        Ok(Self::claim_drive(&mut self.entries()?, letter))
    }

    /// Removes the first matching drive snapshot from a listing, leaving all other rows in order.
    pub fn claim_drive(entries: &mut Vec<SnapshotEntry>, letter: char) -> Option<SnapshotEntry> {
        if !letter.is_ascii_alphabetic() {
            return None;
        }
        let stem = letter.to_string();
        entries
            .iter()
            .position(|entry| {
                entry
                    .file_stem()
                    .is_some_and(|name| name.eq_ignore_ascii_case(&stem))
            })
            .map(|index| entries.remove(index))
    }
}

#[cfg(test)]
mod tests;
