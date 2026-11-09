# Review notes: add-ntfs-parser

## Accepted amendments

D1 uses workspace `unsafe_code = "deny"` so the test-only counting allocator in `tests/no_alloc.rs` can allow unsafe code locally on its `GlobalAlloc` implementation. The allocator delegates to `System` with SAFETY comments. The parser retains `#![forbid(unsafe_code)]`.

D10 uses manifest schema version 2 with full `entries[]` for meaningful files and `fill[]` summaries for disposable children of `/fill` and `/growth`. These are separate sets because they have different parents and name lengths. Numeric arrays and each entry occupy one line. Rust rejects unknown fields in every manifest structure, including filler summaries. Each JSON file is limited to 256 KiB by `tests/budget.rs` and generation.

The deterministic conversion command is `deno run --allow-read=tests/fixtures/ntfs --allow-write=tests/fixtures/ntfs scripts/fixtures/compact.ts`. It shares summarization and serialization with the generator. Converted sizes are basic-4k 96,578 bytes, frag-512 11,579 bytes, and 4kn-64k 1,906 bytes. Compressed images total 928,331 bytes, below 1 MiB; their bytes were preserved. The fragmentation manifest retains four full entries and summarizes 5,337 fill files and 1,500 growth files.

## Verification

The user completed task 8.4 with a successful sudo fixture-generation run and all image postconditions. Committing those files is deferred to the user after review; no commit or staging operation was performed.

`cargo test -p adamite-ntfs --test fixtures` passes on Linux for all three images. It verifies independent geometry and MFT extents, every full entry, filler membership/count/range/parent/name, extension references, root parent 5:5, the deleted record, and zero Corrupt records throughout every stitched MFT. Both synthetic and real fixture hot paths pass the counting-allocator tests.

The Windows half of task 8.6 will be verified by the existing Windows CI job after the user reviews and pushes the commit. It has not been executed locally; the user explicitly approved marking 8.6 complete after Linux verification.

`deno task fuzz-corpus` generates deterministic seeds using independent manifest extents and `zstd -d`. Per-target initial seed sizes are 1,536 / 65,536 / 65,464 / 5,344 / 65,536 bytes for boot_sector / file_record / attributes / runlist / typed_values. All five task 9.2 runs used `-max_total_time=30 -max_len=262144 -timeout=2 -rss_limit_mb=2048` and exited successfully after 31 seconds. Execution counts were 13,782,399 / 860,756 / 269,509 / 4,292,320 / 1,894,964. No crash was found, so task 9.3 required no crash-derived regression. The scenario audit added explicit normal/unknown attribute ordering, compressed-size, and real-fixture allocation coverage.

Full local gate: `deno task check --fuzz` passed, including both feature sets with 1,024 property cases, documentation, both thumbv7em builds, dependency policy, Deno checks, all five fuzz builds, and another 30-second smoke per target with no crash. Deterministic seeds were restored afterward to keep every committed corpus within 64 KiB. Strict OpenSpec validation passed with no unmapped scenarios.

## Scenario coverage

Every scenario below maps to a named deterministic test, property test, or compile-fail doctest. Property tests run with `PROPTEST_CASES=1024` in the full gate. Test references are relative to `crates/adamite-ntfs/tests/` except for the two `record::FileRecord` rustdoc tests.

### volume-geometry

[Spec](specs/ntfs/volume-geometry/spec.md): 17 scenarios.

| Scenario | Named tests |
|---|---|
| Valid boot sector with trailing bytes | [geometry.rs](../../../crates/adamite-ntfs/tests/geometry.rs)::`suffix_and_ignored_bytes_do_not_change_geometry` |
| Truncated input | [geometry.rs](../../../crates/adamite-ntfs/tests/geometry.rs)::`every_short_input_is_truncated_and_every_identification_byte_is_checked` |
| Wrong OEM identifier or end signature | [geometry.rs](../../../crates/adamite-ntfs/tests/geometry.rs)::`every_short_input_is_truncated_and_every_identification_byte_is_checked` |
| Literal sectors per cluster | [geometry.rs](../../../crates/adamite-ntfs/tests/geometry.rs)::`documented_windows_and_real_mkntfs_golden_sectors` |
| Exponent-encoded large cluster | [geometry.rs](../../../crates/adamite-ntfs/tests/geometry.rs)::`exponent_golden_cases_and_positive_record_encoding` |
| Maximum cluster size | [geometry.rs](../../../crates/adamite-ntfs/tests/geometry.rs)::`exponent_golden_cases_and_positive_record_encoding` |
| Cluster size above the limit | [geometry.rs](../../../crates/adamite-ntfs/tests/geometry.rs)::`exponent_golden_cases_and_positive_record_encoding` |
| Invalid sector or cluster encodings | [geometry.rs](../../../crates/adamite-ntfs/tests/geometry.rs)::`exhaustive_sector_cluster_and_record_encodings` |
| Negative encoding | [geometry.rs](../../../crates/adamite-ntfs/tests/geometry.rs)::`documented_windows_and_real_mkntfs_golden_sectors` |
| Positive encoding with small clusters | [geometry.rs](../../../crates/adamite-ntfs/tests/geometry.rs)::`exponent_golden_cases_and_positive_record_encoding` |
| Zero or out-of-range encoding | [geometry.rs](../../../crates/adamite-ntfs/tests/geometry.rs)::`exhaustive_sector_cluster_and_record_encodings` |
| Valid MFT location | [geometry.rs](../../../crates/adamite-ntfs/tests/geometry.rs)::`sector_counts_and_mft_locations_are_bounded_by_complete_clusters` |
| MFT beyond volume end | [geometry.rs](../../../crates/adamite-ntfs/tests/geometry.rs)::`sector_counts_and_mft_locations_are_bounded_by_complete_clusters` |
| Zero or overflowing sector count | [geometry.rs](../../../crates/adamite-ntfs/tests/geometry.rs)::`sector_counts_and_mft_locations_are_bounded_by_complete_clusters` |
| Range inside the volume | [geometry.rs](../../../crates/adamite-ntfs/tests/geometry.rs)::`cluster_ranges_match_u128_model` |
| Range past the end or overflowing | [geometry.rs](../../../crates/adamite-ntfs/tests/geometry.rs)::`cluster_ranges_match_u128_model` |
| Arbitrary bytes | [geometry.rs](../../../crates/adamite-ntfs/tests/geometry.rs)::`arbitrary_boot_bytes_are_total` |

### file-record

[Spec](specs/ntfs/file-record/spec.md): 43 scenarios.

| Scenario | Named tests |
|---|---|
| Raw round trip | [file_ref.rs](../../../crates/adamite-ntfs/tests/file_ref.rs)::`raw_and_bytes_round_trip` |
| Construction bounds | [file_ref.rs](../../../crates/adamite-ntfs/tests/file_ref.rs)::`construction_boundaries` |
| Null reference | [file_ref.rs](../../../crates/adamite-ntfs/tests/file_ref.rs)::`only_the_all_zero_reference_is_null` |
| Root constant | [file_ref.rs](../../../crates/adamite-ntfs/tests/file_ref.rs)::`well_known_record_numbers` |
| Peek matches post-fixup header | [file_record.rs](../../../crates/adamite-ntfs/tests/file_record.rs)::`peek_matches_fixed_header` |
| Peek on short input | [file_record.rs](../../../crates/adamite-ntfs/tests/file_record.rs)::`peek_checks_layout_specific_truncation` |
| NTFS 3.1 record | [file_record.rs](../../../crates/adamite-ntfs/tests/file_record.rs)::`peek_decodes_every_fixed_field_without_mutation` |
| NTFS 3.0 record | [file_record.rs](../../../crates/adamite-ntfs/tests/file_record.rs)::`ntfs30_never_interprets_record_number_bytes` |
| Valid header | [file_record.rs](../../../crates/adamite-ntfs/tests/file_record.rs)::`classification_scenarios_and_header_validation` |
| Inconsistent sizes | [file_record.rs](../../../crates/adamite-ntfs/tests/file_record.rs)::`classification_scenarios_and_header_validation` |
| Successful fixup of a 1 KiB record | [file_record.rs](../../../crates/adamite-ntfs/tests/file_record.rs)::`fixup_restores_only_trailers_on_all_supported_sizes_and_usns` |
| 4 KiB record on a 4Kn volume | [file_record.rs](../../../crates/adamite-ntfs/tests/file_record.rs)::`fixup_restores_only_trailers_on_all_supported_sizes_and_usns`; [fixtures.rs](../../../crates/adamite-ntfs/tests/fixtures.rs)::`native_4kn_geometry_and_records_match_the_manifest` |
| Torn write | [file_record.rs](../../../crates/adamite-ntfs/tests/file_record.rs)::`every_torn_tail_reports_its_stride_without_any_writes` |
| Bad update-sequence metadata | [file_record.rs](../../../crates/adamite-ntfs/tests/file_record.rs)::`fixup_metadata_boundaries_and_validation_order` |
| Parsing requires fixup | [record::FileRecord doctest, line 212](../../../crates/adamite-ntfs/src/record.rs#L212) |
| Views borrow the buffer | [record::FileRecord doctest, line 199](../../../crates/adamite-ntfs/src/record.rs#L199) |
| Never-used record | [file_record.rs](../../../crates/adamite-ntfs/tests/file_record.rs)::`classification_scenarios_and_header_validation` |
| Chkdsk-marked record | [file_record.rs](../../../crates/adamite-ntfs/tests/file_record.rs)::`classification_scenarios_and_header_validation` |
| Base record | [file_record.rs](../../../crates/adamite-ntfs/tests/file_record.rs)::`classification_scenarios_and_header_validation` |
| Unknown signature | [file_record.rs](../../../crates/adamite-ntfs/tests/file_record.rs)::`classification_scenarios_and_header_validation` |
| Deleted record | [file_record.rs](../../../crates/adamite-ntfs/tests/file_record.rs)::`classification_scenarios_and_header_validation`; [fixtures.rs](../../../crates/adamite-ntfs/tests/fixtures.rs)::`basic_4k_matches_every_manifest_oracle` |
| Extension record | [file_record.rs](../../../crates/adamite-ntfs/tests/file_record.rs)::`classification_scenarios_and_header_validation` |
| Base reference with record 0 and non-zero sequence | [file_record.rs](../../../crates/adamite-ntfs/tests/file_record.rs)::`classification_scenarios_and_header_validation` |
| Torn in-use record | [file_record.rs](../../../crates/adamite-ntfs/tests/file_record.rs)::`classification_scenarios_and_header_validation` |
| Unknown flags | [file_record.rs](../../../crates/adamite-ntfs/tests/file_record.rs)::`unknown_flags_and_zero_counters_are_kept` |
| Normal record | [attributes.rs](../../../crates/adamite-ntfs/tests/attributes.rs)::`normal_records_and_unknown_attributes_between_known_types` |
| Attribute crossing bytes_used | [attributes.rs](../../../crates/adamite-ntfs/tests/attributes.rs)::`used_bytes_end_marker_and_slack_bound_iteration` |
| Missing end marker | [attributes.rs](../../../crates/adamite-ntfs/tests/attributes.rs)::`used_bytes_end_marker_and_slack_bound_iteration` |
| Slack does not matter | [attributes.rs](../../../crates/adamite-ntfs/tests/attributes.rs)::`suffix_and_used_byte_boundaries_are_independent` |
| Zero-length attribute | [attributes.rs](../../../crates/adamite-ntfs/tests/attributes.rs)::`zero_length_attribute_errors_once` |
| Fused after error | [attributes.rs](../../../crates/adamite-ntfs/tests/attributes.rs)::`malformed_headers_names_and_values_error_once` |
| Unknown attribute type | [attributes.rs](../../../crates/adamite-ntfs/tests/attributes.rs)::`normal_records_and_unknown_attributes_between_known_types` |
| Named attribute | [attributes.rs](../../../crates/adamite-ntfs/tests/attributes.rs)::`basic_attribute_sequence_names_and_resident_values` |
| Name out of bounds | [attributes.rs](../../../crates/adamite-ntfs/tests/attributes.rs)::`malformed_headers_names_and_values_error_once` |
| Resident value | [attributes.rs](../../../crates/adamite-ntfs/tests/attributes.rs)::`normal_records_and_unknown_attributes_between_known_types` |
| Value out of bounds | [attributes.rs](../../../crates/adamite-ntfs/tests/attributes.rs)::`malformed_headers_names_and_values_error_once` |
| Compressed stream header | [attributes.rs](../../../crates/adamite-ntfs/tests/attributes.rs)::`compressed_and_sparse_extended_headers_report_compressed_size` |
| Short header for a compressed stream | [attributes.rs](../../../crates/adamite-ntfs/tests/attributes.rs)::`nonresident_header_and_numeric_failures_are_specific` |
| Reserved bytes after compression unit | [attributes.rs](../../../crates/adamite-ntfs/tests/attributes.rs)::`continuation_sizes_and_reserved_bytes_are_ignored` |
| Continuation segment | [attributes.rs](../../../crates/adamite-ntfs/tests/attributes.rs)::`continuation_sizes_and_reserved_bytes_are_ignored` |
| Negative size in first segment | [attributes.rs](../../../crates/adamite-ntfs/tests/attributes.rs)::`nonresident_header_and_numeric_failures_are_specific` |
| Arbitrary record bytes | [file_record.rs](../../../crates/adamite-ntfs/tests/file_record.rs)::`arbitrary_record_operations_are_total_and_errors_are_atomic` |
| No allocation in the hot path | [no_alloc.rs](../../../crates/adamite-ntfs/tests/no_alloc.rs)::`record_attribute_and_runlist_hot_path_does_not_allocate`; [no_alloc.rs](../../../crates/adamite-ntfs/tests/no_alloc.rs)::`real_fixture_records_do_not_allocate_during_classification_or_iteration` |

### attribute-decoding

[Spec](specs/ntfs/attribute-decoding/spec.md): 27 scenarios.

| Scenario | Named tests |
|---|---|
| Wrong attribute type | [typed_values.rs](../../../crates/adamite-ntfs/tests/typed_values.rs)::`typed_metadata_decoders_reject_wrong_type_and_storage` |
| Wrong storage form | [typed_values.rs](../../../crates/adamite-ntfs/tests/typed_values.rs)::`typed_metadata_decoders_reject_wrong_type_and_storage` |
| NTFS 3.x layout | [typed_values.rs](../../../crates/adamite-ntfs/tests/typed_values.rs)::`standard_information_legacy_extended_and_lenient_tail` |
| Legacy layout | [typed_values.rs](../../../crates/adamite-ntfs/tests/typed_values.rs)::`standard_information_legacy_extended_and_lenient_tail` |
| Trailing bytes tolerated | [typed_values.rs](../../../crates/adamite-ntfs/tests/typed_values.rs)::`standard_information_legacy_extended_and_lenient_tail` |
| Invalid length | [typed_values.rs](../../../crates/adamite-ntfs/tests/typed_values.rs)::`standard_information_legacy_extended_and_lenient_tail` |
| Win32 name | [typed_values.rs](../../../crates/adamite-ntfs/tests/typed_values.rs)::`file_names_preserve_namespace_metadata_and_raw_units` |
| Name exceeds value | [typed_values.rs](../../../crates/adamite-ntfs/tests/typed_values.rs)::`file_names_preserve_namespace_metadata_and_raw_units` |
| Invalid namespace or empty name | [typed_values.rs](../../../crates/adamite-ntfs/tests/typed_values.rs)::`file_names_preserve_namespace_metadata_and_raw_units` |
| Supplementary character | [utf16.rs](../../../crates/adamite-ntfs/tests/utf16.rs)::`supplementary_character_and_nul_are_preserved` |
| Unpaired surrogate | [utf16.rs](../../../crates/adamite-ntfs/tests/utf16.rs)::`unpaired_surrogate_followed_by_ascii` |
| Agreement with the standard decoder | [utf16.rs](../../../crates/adamite-ntfs/tests/utf16.rs)::`units_and_decoders_match_standard` |
| Lossy string | [utf16.rs](../../../crates/adamite-ntfs/tests/utf16.rs)::`string_helpers_equal_iterator_output` |
| Alternate data stream | [typed_values.rs](../../../crates/adamite-ntfs/tests/typed_values.rs)::`data_streams_are_reported_separately_with_resident_lengths` |
| Non-resident main stream | [typed_values.rs](../../../crates/adamite-ntfs/tests/typed_values.rs)::`nonresident_data_and_directory_index_preserve_segment_descriptors` |
| Directory index | [typed_values.rs](../../../crates/adamite-ntfs/tests/typed_values.rs)::`nonresident_data_and_directory_index_preserve_segment_descriptors`; [fixtures.rs](../../../crates/adamite-ntfs/tests/fixtures.rs)::`basic_4k_matches_every_manifest_oracle` |
| Symbolic link tag | [typed_values.rs](../../../crates/adamite-ntfs/tests/typed_values.rs)::`reparse_values_preserve_tags_and_all_payload_bytes` |
| Declared length too large | [typed_values.rs](../../../crates/adamite-ntfs/tests/typed_values.rs)::`reparse_values_preserve_tags_and_all_payload_bytes` |
| Value too large | [typed_values.rs](../../../crates/adamite-ntfs/tests/typed_values.rs)::`reparse_values_preserve_tags_and_all_payload_bytes` |
| Least-significant bit first | [typed_values.rs](../../../crates/adamite-ntfs/tests/typed_values.rs)::`bitmap_scenarios_and_fused_iteration` |
| Padding bits ignored | [typed_values.rs](../../../crates/adamite-ntfs/tests/typed_values.rs)::`bitmap_scenarios_and_fused_iteration` |
| Logical length too large | [typed_values.rs](../../../crates/adamite-ntfs/tests/typed_values.rs)::`bitmap_scenarios_and_fused_iteration` |
| $MFT data in an extension record | [attribute_list.rs](../../../crates/adamite-ntfs/tests/attribute_list.rs)::`mft_extension_reference_and_resident_attribute_adapter` |
| Truncated entry | [attribute_list.rs](../../../crates/adamite-ntfs/tests/attribute_list.rs)::`lengths_names_negative_vcns_and_short_zero_tails` |
| Zero-length entry | [attribute_list.rs](../../../crates/adamite-ntfs/tests/attribute_list.rs)::`lengths_names_negative_vcns_and_short_zero_tails` |
| Conflicting cached size | [typed_values.rs](../../../crates/adamite-ntfs/tests/typed_values.rs)::`cached_file_name_size_and_data_size_remain_independent` |
| Arbitrary value bytes | [typed_values.rs](../../../crates/adamite-ntfs/tests/typed_values.rs)::`every_typed_decoder_is_total` |

### runlist

[Spec](specs/ntfs/runlist/spec.md): 18 scenarios.

| Scenario | Named tests |
|---|---|
| Empty stream | [runlist.rs](../../../crates/adamite-ntfs/tests/runlist.rs)::`documented_single_run_and_empty_stream` |
| Invalid range | [runlist.rs](../../../crates/adamite-ntfs/tests/runlist.rs)::`range_rules_include_literal_zero_and_empty_stream` |
| Single run | [runlist.rs](../../../crates/adamite-ntfs/tests/runlist.rs)::`documented_single_run_and_empty_stream` |
| Negative delta | [runlist.rs](../../../crates/adamite-ntfs/tests/runlist.rs)::`documented_negative_delta_and_other_golden_runs` |
| Unsigned length with high bit set | [runlist.rs](../../../crates/adamite-ntfs/tests/runlist.rs)::`documented_negative_delta_and_other_golden_runs` |
| Eight-byte fields | [runlist.rs](../../../crates/adamite-ntfs/tests/runlist.rs)::`documented_negative_delta_and_other_golden_runs` |
| Invalid widths | [runlist.rs](../../../crates/adamite-ntfs/tests/runlist.rs)::`every_width_header_and_truncated_field_is_checked` |
| Physical, sparse, physical | [runlist.rs](../../../crates/adamite-ntfs/tests/runlist.rs)::`sparse_base_zero_lcn_and_extension_segment` |
| Leading sparse run | [runlist.rs](../../../crates/adamite-ntfs/tests/runlist.rs)::`sparse_base_zero_lcn_and_extension_segment` |
| Extension segment | [runlist.rs](../../../crates/adamite-ntfs/tests/runlist.rs)::`sparse_base_zero_lcn_and_extension_segment` |
| Negative resulting LCN | [runlist.rs](../../../crates/adamite-ntfs/tests/runlist.rs)::`coverage_zero_lengths_overflow_and_negative_lcns` |
| Delta overflow | [runlist.rs](../../../crates/adamite-ntfs/tests/runlist.rs)::`near_limit_overflow_and_negative_lcns_are_rejected` |
| Under-coverage | [runlist.rs](../../../crates/adamite-ntfs/tests/runlist.rs)::`padding_is_ignored_and_coverage_must_be_exact` |
| Truncated pair | [runlist.rs](../../../crates/adamite-ntfs/tests/runlist.rs)::`every_width_header_and_truncated_field_is_checked` |
| Padding after terminator | [runlist.rs](../../../crates/adamite-ntfs/tests/runlist.rs)::`padding_is_ignored_and_coverage_must_be_exact` |
| Zero-length run | [runlist.rs](../../../crates/adamite-ntfs/tests/runlist.rs)::`coverage_zero_lengths_overflow_and_negative_lcns` |
| Arbitrary input | [runlist.rs](../../../crates/adamite-ntfs/tests/runlist.rs)::`arbitrary_input_is_bounded_and_fused` |
| Run past volume end | [runlist.rs](../../../crates/adamite-ntfs/tests/runlist.rs)::`geometry_checks_accept_sparse_and_exact_end_but_reject_overrun` |

Coverage audit: 105/105 scenarios mapped; no scenario is unmapped.

## Cross-review

1. Renamed the fallible run collector to `Runlist::collect_runs` and restored ordinary `Iterator::collect` calls with both feature sets.
2. Made fuzz preflight fail for missing, empty, or unusable corpus directories. The finding that corpora were missing was invalid: all five corpora were present and were retained.
3. Wrapped attribute-list type/storage errors in `AttributeListError::Value` with a `Copy` `ValueError` source, shared the private `ensure_type` check, and updated ScanStats documentation and source/size tests.
4. Shared the record-size and USA-offset predicates between record validation and fixup, preserving and documenting the error variants reported by each path.
5. Shared fused iterator completion and one-error termination through the private `finish_step` helper.
6. Removed the bytes module-wide dead-code expectation and deleted unused `align_up`.
7. Defined typed-value wire selectors once as documented Rust constants; extraction references those names, and `every_typed_seed_decodes_with_its_named_selector` validates every generated seed with its selected decoder and requires all eight selectors. The check selects extractor seeds by their SHA-256 filenames so later fuzz mutations do not invalidate repeated checks.

Cross-review validation: `deno task check --fuzz` and `openspec validate add-ntfs-parser --strict` passed. Targeted tests passed with both feature sets, all 10 Deno tests passed, and every generated typed seed decoded successfully before and after fuzzing. All five final 30-second smoke runs were clean. Deterministic corpora were restored within their 64 KiB limits. All 45 tasks remain ticked.
