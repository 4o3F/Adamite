//! Record and diagnostic counters owned by a completed scan.

/// Scan counters, independent of transport chunk size and response arrival order.
///
/// Counter updates use checked arithmetic as ingestion and tree phases are implemented.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ScanStats {
    /// MFT ordinals classified exactly once during streaming.
    pub records_total: u64,
    /// Base and extension records classified as in use, before later rejection.
    pub records_in_use: u64,
    /// Records classified as bases.
    pub base_records: u64,
    /// Records classified as extensions.
    pub extension_records: u64,
    /// Uniformly unused records.
    pub unused: u64,
    /// Valid headers with IN_USE clear.
    pub deleted: u64,
    /// Records marked BAAD.
    pub baad: u64,
    /// Ordinary records discarded after parser failures.
    pub corrupt_records: u64,
    /// Mismatched protected trailers.
    pub bad_fixup: u64,
    /// Invalid update-sequence array metadata.
    pub invalid_fixup_metadata: u64,
    /// Unrecognized record signatures.
    pub invalid_signature: u64,
    /// Invalid fixed record headers.
    pub corrupt_headers: u64,
    /// Invalid attribute headers or extents.
    pub corrupt_attributes: u64,
    /// Typed values with the wrong type or storage form.
    pub type_storage_mismatches: u64,
    /// Invalid typed attribute values.
    pub corrupt_values: u64,
    /// Invalid mapping-pair encodings or coverage.
    pub corrupt_runlists: u64,
    /// Physical runs outside volume bounds.
    pub out_of_volume_extents: u64,
    /// Malformed attribute lists.
    pub invalid_attribute_lists: u64,
    /// Unrepresentable or invalid file references.
    pub invalid_file_refs: u64,
    /// Invalid surrogate sequences encountered while decoding names.
    pub invalid_utf16: u64,
    /// Patches with missing or stale bases.
    pub rejected_extensions: u64,
    /// Streams rejected while merging segments.
    pub corrupt_streams: u64,
    /// Nodes reparented because of unresolved parents or cycles.
    pub orphans: u64,
    /// Directory nodes reparented to break cycles.
    pub cycle_nodes: u64,
    /// Non-primary hard-link nodes.
    pub hardlink_duplicates: u64,
    /// Distinct base records grouped as NTFS metadata.
    pub metafiles: u64,
}
