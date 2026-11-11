//! Record and diagnostic counters owned by a completed scan.
//!
//! Streaming ingestion classifies each MFT ordinal once. Corrupt classifications increment `corrupt_records` plus the diagnostic below. Deleted records still increment `deleted`; an inconsistent stored record number also increments `corrupt_records` and `corrupt_headers`. In-use bases/extensions increment their classification counters before identity or contribution validation.
//!
//! | Streaming failure | Diagnostic counter |
//! |---|---|
//! | `RecordError::Fixup` with `FixupError::TrailerMismatch` | `bad_fixup` |
//! | Other `RecordError::Fixup` | `invalid_fixup_metadata` |
//! | `RecordError::InvalidSignature` | `invalid_signature` |
//! | Other `RecordError`, or a stored record number inconsistent with its ordinal | `corrupt_headers` |
//! | Attribute iteration/header failure | `corrupt_attributes` |
//! | `ValueError::WrongAttributeType` or `WrongStorageForm` | `type_storage_mismatches` |
//! | Other typed-value failure | `corrupt_values` |
//! | Runlist iteration/coverage failure | `corrupt_runlists` |
//! | Run geometry validation failure | `out_of_volume_extents` |
//! | Resident attribute-list construction or entry failure | `invalid_attribute_lists` |
//!
//! Contribution decoding discards the whole contribution on the first failure, incrementing its diagnostic and `corrupt_records`. Resident attribute-list errors all use `invalid_attribute_lists` in this path. Failure to construct a streamed `FileRef` increments `invalid_file_refs` and terminates with [`ScanError::FileRef`](crate::error::ScanError::FileRef). These rules are implemented in `mft/ingest.rs::Ingest::record_error` and `mft/ingest/decode.rs::DecodeError::account`.
//!
//! During merge, an invalid extension base/source identity increments both `rejected_extensions` and `invalid_file_refs`. Conflicting standard-information values or reparse tags discard the base and increment `corrupt_values` and `corrupt_records`. Each invalid stream group increments `corrupt_streams`; rejecting any group discards the base and increments `corrupt_records` once. Allocation, arithmetic and required-root failures terminate through [`ScanError`](crate::error::ScanError).
//!
//! Tree name conversion increments `invalid_utf16` once per unpaired surrogate replaced in an emitted name, after name selection and deduplication. Names are retained as UTF-16 units until that conversion; the preliminary UTF-8 length calculation does not increment counters. Bitmap decoding has no scan counter path, and odd-length attribute names are rejected by attribute/value validation before tree conversion.
//!
//! Boot and MFT bootstrap errors terminate the scan through [`ScanError`](crate::error::ScanError), rather than producing streaming corruption counts. Boot decoding uses `Geometry { source: BootError }`; bootstrap record parsing maps `RecordError::Fixup { source }` to `Fixup { source }` and other record errors to `Record { source }`. Header validation precedes fixup, so a record with both failures reports the header error. Attribute, runlist and attribute-list failures use the corresponding `Attribute`, `Runlist` and `AttributeList` variants with their parser sources. Invalid physical bootstrap extents use `OutOfVolume`; identity/storage constraints, list size limits and stalled discovery use `Bootstrap`, `AttributeListTooLarge` and `BootstrapIncomplete`, respectively. Nested parser error sources and their Display text are preserved.

/// Scan counters, independent of transport chunk size and response arrival order.
///
/// Counter updates use checked arithmetic; corruption categories may overlap.
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
    /// Record-level parse, identity or merge failures; may overlap the deleted count.
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

impl ScanStats {
    /// Returns all 26 counters in declaration order, with their snake_case field names.
    ///
    /// This order is also the counter order stored in the Stats section of canonical .adm v1.
    #[must_use]
    pub const fn counters(&self) -> [(&'static str, u64); 26] {
        [
            ("records_total", self.records_total),
            ("records_in_use", self.records_in_use),
            ("base_records", self.base_records),
            ("extension_records", self.extension_records),
            ("unused", self.unused),
            ("deleted", self.deleted),
            ("baad", self.baad),
            ("corrupt_records", self.corrupt_records),
            ("bad_fixup", self.bad_fixup),
            ("invalid_fixup_metadata", self.invalid_fixup_metadata),
            ("invalid_signature", self.invalid_signature),
            ("corrupt_headers", self.corrupt_headers),
            ("corrupt_attributes", self.corrupt_attributes),
            ("type_storage_mismatches", self.type_storage_mismatches),
            ("corrupt_values", self.corrupt_values),
            ("corrupt_runlists", self.corrupt_runlists),
            ("out_of_volume_extents", self.out_of_volume_extents),
            ("invalid_attribute_lists", self.invalid_attribute_lists),
            ("invalid_file_refs", self.invalid_file_refs),
            ("invalid_utf16", self.invalid_utf16),
            ("rejected_extensions", self.rejected_extensions),
            ("corrupt_streams", self.corrupt_streams),
            ("orphans", self.orphans),
            ("cycle_nodes", self.cycle_nodes),
            ("hardlink_duplicates", self.hardlink_duplicates),
            ("metafiles", self.metafiles),
        ]
    }
}
