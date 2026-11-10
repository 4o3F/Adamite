//! Owned entries and graph-specific snapshot build input construction.

use crate::{
    error::ScanError,
    snapshot::{NodeFlags, ResolvedNode},
};
use adamite_ntfs::record::reference::FileRef;
use std::fmt::Write;

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct EntryName {
    pub parent: FileRef,
    pub namespace: u8,
    pub name: Vec<u16>,
}

/// Owned boundary consumed by generic linking and aggregation.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct EntryRecord {
    pub file_ref: FileRef,
    pub attrs: u32,
    pub mtime: u64,
    pub size: u64,
    pub ads_size: u64,
    pub alloc: u64,
    pub names: Vec<EntryName>,
    pub nlink: u16,
    pub reparse_tag: Option<u32>,
    pub flags: NodeFlags,
}

impl EntryRecord {
    /// Select aliases before decoding; equal parent/name links keep the smallest namespace.
    pub(super) fn select_names(&mut self) {
        let mut names = std::mem::take(&mut self.names);
        names.sort_unstable_by(|a, b| {
            (
                a.parent.record(),
                a.parent.sequence(),
                a.namespace == 2,
                a.namespace,
                &a.name,
            )
                .cmp(&(
                    b.parent.record(),
                    b.parent.sequence(),
                    b.namespace == 2,
                    b.namespace,
                    &b.name,
                ))
        });
        let mut parent = None;
        let mut non_dos = false;
        names.retain(|name| {
            if parent != Some(name.parent) {
                parent = Some(name.parent);
                non_dos = false;
            }
            if name.namespace != 2 {
                non_dos = true;
            }
            name.namespace != 2 || !non_dos
        });
        names.sort_unstable_by(|a, b| {
            (a.parent.record(), a.parent.sequence(), &a.name, a.namespace).cmp(&(
                b.parent.record(),
                b.parent.sequence(),
                &b.name,
                b.namespace,
            ))
        });
        names.dedup_by(|a, b| a.parent == b.parent && a.name == b.name);
        names.sort_unstable_by(|a, b| {
            (a.parent.record(), &a.name, a.namespace, a.parent.sequence()).cmp(&(
                b.parent.record(),
                &b.name,
                b.namespace,
                b.parent.sequence(),
            ))
        });
        self.names = names;
    }

    pub(super) fn fallback_name(&self) -> Result<String, ScanError> {
        let record = self.file_ref.record();
        let mut text = String::new();
        // A 48-bit ordinal needs at most 15 digits, plus the ASCII label and delimiters.
        text.try_reserve_exact(32)?;
        write!(&mut text, "<record {record}>")
            .map_err(|_| ScanError::link("could not format unnamed record"))?;
        Ok(text)
    }

    pub(super) fn resolved_node(&self, parent_reference: Option<FileRef>) -> ResolvedNode {
        ResolvedNode {
            parent: 0,
            file_ref: Some(self.file_ref),
            original_parent: parent_reference,
            name_off: 0,
            name_len: 0,
            flags: self.flags,
            attrs: self.attrs,
            mtime: self.mtime,
            size: self.size,
            ads_size: self.ads_size,
            alloc: self.alloc,
            nlink: self.nlink,
            reparse_tag: self.reparse_tag,
        }
    }
}

impl ResolvedNode {
    pub(super) fn synthetic() -> Self {
        let mut flags = NodeFlags::DIRECTORY;
        flags.insert(NodeFlags::SYNTHETIC);
        ResolvedNode {
            parent: 0,
            file_ref: None,
            original_parent: None,
            name_off: 0,
            name_len: 0,
            flags,
            attrs: 0,
            mtime: 0,
            size: 0,
            ads_size: 0,
            alloc: 0,
            nlink: 0,
            reparse_tag: None,
        }
    }
}
