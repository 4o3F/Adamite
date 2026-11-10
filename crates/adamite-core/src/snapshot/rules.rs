//! Shared deterministic ranking and filename-extension rules.

use super::{Node, NodeId, SnapshotError};
use adamite_ntfs::record::reference::FileRef;
use std::cmp::Ordering;

const TOP_FILES: usize = 100;

#[derive(Clone, Copy)]
pub(super) enum Metric {
    SelfAlloc,
    TotalAlloc,
}

pub(super) struct Ranking<'a> {
    pub(super) nodes: &'a [Node],
    pub(super) names: &'a [u8],
    pub(super) refs: &'a [Option<FileRef>],
}

impl Ranking<'_> {
    fn node(&self, id: NodeId) -> Option<&Node> {
        usize::try_from(id.raw())
            .ok()
            .and_then(|id| self.nodes.get(id))
    }

    pub(super) fn name(&self, id: NodeId) -> Option<&str> {
        let node = self.node(id)?;
        let start = usize::try_from(node.name_off).ok()?;
        let end = start.checked_add(usize::from(node.name_len))?;
        std::str::from_utf8(self.names.get(start..end)?).ok()
    }

    // Order file identities by record number, then sequence number.
    fn reference(&self, id: NodeId) -> Option<(u64, u16)> {
        usize::try_from(id.raw())
            .ok()
            .and_then(|id| self.refs.get(id))
            .copied()
            .flatten()
            .map(|reference| (reference.record(), reference.sequence()))
    }

    pub(super) fn compare(&self, left: NodeId, right: NodeId, metric: Metric) -> Ordering {
        let alloc = |id| {
            self.node(id).map(|node| match metric {
                Metric::SelfAlloc => node.alloc,
                Metric::TotalAlloc => node.total_alloc,
            })
        };
        alloc(right)
            .cmp(&alloc(left))
            // Valid names reproduce their original UTF-16 ordering without a scratch allocation.
            // Lossy names compare their replacement text, then stable file references and ids.
            .then_with(|| {
                self.name(left)
                    .into_iter()
                    .flat_map(str::encode_utf16)
                    .cmp(self.name(right).into_iter().flat_map(str::encode_utf16))
            })
            .then_with(|| self.reference(left).cmp(&self.reference(right)))
            .then_with(|| left.cmp(&right))
    }

    pub(super) fn select_top(
        &self,
        top: &mut Vec<NodeId>,
        id: NodeId,
    ) -> Result<(), SnapshotError> {
        let position = top
            .binary_search_by(|other| self.compare(*other, id, Metric::SelfAlloc))
            .unwrap_or_else(|position| position);
        if position < TOP_FILES {
            if top.len() == TOP_FILES {
                top.pop();
            }
            top.try_reserve(1)?;
            top.insert(position, id);
        }
        Ok(())
    }
}

pub(super) struct Extension<'a>(&'a str);

impl<'a> Extension<'a> {
    pub(super) fn from_name(name: &'a str) -> Self {
        Self(match name.rsplit_once('.') {
            Some((stem, suffix)) if !stem.is_empty() && !suffix.is_empty() => suffix,
            _ => "",
        })
    }

    pub(super) fn compare_folded(&self, folded: &str) -> Ordering {
        folded
            .bytes()
            .cmp(self.0.bytes().map(|byte| byte.to_ascii_lowercase()))
    }

    pub(super) fn folded(&self) -> Result<String, SnapshotError> {
        let mut key = String::new();
        key.try_reserve_exact(self.0.len())?;
        for character in self.0.chars() {
            key.push(character.to_ascii_lowercase());
        }
        Ok(key)
    }
}
