# Task 04 — Split `crates/enterprise/src/enterprise/saml.rs` into a `saml/` module

## Goal

Split `crates/enterprise/src/enterprise/saml.rs` (1,991 lines) into
`crates/enterprise/src/saml/{mod,assertion,verify,discovery,tests}.rs` — behavior-preserving;
`cargo build` + `cargo test -p rume-enterprise --features saml` green; the core umbrella test
(`cargo test -p rume --features enterprise`) stays green.

## Files touched

- `crates/enterprise/src/enterprise/saml.rs` — deleted.
- `crates/enterprise/src/saml/mod.rs` — module docs (saml.rs:1-26, kept verbatim); `mod assertion; mod
  discovery; mod verify;`; `pub use assertion::{parse_iso8601_strict, SamlAssertion, SamlParser,
  SamlSubjectConfirmationData};` + `pub use verify::{InMemorySamlReplayCache, SamlError, SamlReplayCache,
  SamlSignatureVerifier, SamlValidationConfig};`; `#[cfg(test)] mod tests;` → `saml/tests.rs`.
- `crates/enterprise/src/saml/assertion.rs` — `is_ns` (36), `XmlNs` (44), `AssertionFields` (52),
  `TextTarget` (60), `ActiveTextCapture` (68), `resolve_general_ref` (80), `parse_iso8601_strict` (294),
  `SamlAssertion` (259), `SamlSubjectConfirmationData` (194), `SamlParser` (318) +
  `SamlParser::{parse_internal, parse_unverified, parse_signed, parse_with_verifier}` (357-961).
- `crates/enterprise/src/saml/discovery.rs` — `impl SamlParser { pub fn parse_entity_id }` (322-353).
- `crates/enterprise/src/saml/verify.rs` — `SamlError` (97-149), `SamlReplayCache` (152-156),
  `InMemorySamlReplayCache` (160-190), `SamlSubjectConfirmationData` — no, in assertion.rs;
  `SamlValidationConfig` (204-241) + `Default`, `SamlSignatureVerifier` (244-255),
  `impl SamlParser { validate (968), validate_strict (989), validate_with_config (1012-1215) }`.
- `crates/enterprise/src/saml/tests.rs` — the `#[cfg(test)] mod saml_parser_and_iso8601_proptests`
  moved verbatim (saml.rs:1218-1991).
- `crates/enterprise/src/enterprise/mod.rs` — drop `#[cfg(feature = "saml")] pub mod saml;`;
  re-export `pub use crate::saml::{InMemorySamlReplayCache, SamlAssertion, SamlError, SamlParser,
  SamlReplayCache, SamlSignatureVerifier, SamlSubjectConfirmationData, SamlValidationConfig,
  parse_iso8601_strict};` (so `rume::enterprise::SamlParser` still resolves via the re-export).
- `crates/enterprise/src/lib.rs` — add `#[cfg(feature = "saml")] pub mod saml;` (module now lives at
  the crate root; `rume_enterprise::saml::…` is the canonical path; nothing else referenced
  `rume_enterprise::enterprise::saml` — verified by grep).

Note on verify.rs: `SamlParser` is defined in assertion.rs; `verify.rs` implements
`impl SamlParser { validate/validate_strict/validate_with_config }` — same module tree, so this
splitting of `impl` blocks across files is fine.

## Deliverable

`rume_enterprise::saml::{SamlParser, SamlAssertion, SamlSubjectConfirmationData,
parse_iso8601_strict}` (assertion) + `rume_enterprise::saml::{SamlError, SamlValidationConfig,
SamlSignatureVerifier, SamlReplayCache, InMemorySamlReplayCache}` (verify) +
`rume_enterprise::saml::SamlParser::parse_entity_id` (discovery) — all re-exported by
`saml/mod.rs`; `rume::enterprise::{SamlParser, …}` unchanged (enterprise/mod.rs re-exports).
Core feature wiring unchanged (`crates/core/Cargo.toml:59,60`: `saml = ["rume-enterprise/saml"]`,
`enterprise = ["saml", …]`; `crates/enterprise/Cargo.toml:32` `saml = ["dep:quick-xml",
"dep:saml-rs", "dep:chrono"]`). No behavior changes.

## Constraints honoured

- `crates/core/tests/integration/enterprise.rs:1` `use rume::enterprise::*;` — resolves via
  `enterprise/mod.rs` re-exports; untouched.
- `crates/core/Cargo.toml:59-60` features unchanged.
- No behavior change; `cargo build` + `cargo test -p rume-enterprise --features saml` green, plus
  `cargo test -p rume --features enterprise` (core umbrella test).

## Named tests (bodies omitted — moved from `crates/enterprise/src/enterprise/saml.rs:1218+`)

From `saml/tests.rs`:
- `parse_unverified_never_panics_on_arbitrary_utf8` (proptest — also calls `SamlParser::parse_entity_id`).
- `parse_iso8601_strict_never_panics_on_arbitrary_or_multibyte_offsets` (proptest).
- `parse_iso8601_strict_converts_valid_rfc3339_instants_and_rejects_pre_epoch` — asserts strict
  RFC 3339 parsing and pre-epoch rejection.
- `parse_iso8601_strict_rejects_out_of_range_offsets_space_separator_and_pre_1970`.
- `signed_mode_rejects_reference_outside_signature_signed_info`.
- `claims_assemble_across_cdata_comments_entities_and_nested_issuer`.
- `unresolved_xml_entity_reference_fails_closed`.
- `nested_attribute_inside_attribute_value_keeps_owning_attribute_name`.
- `signature_subtree_injected_subject_conditions_and_attributes_are_ignored`.
- `signed_mode_rejects_unsigned_response_issuer_fallback_when_assertion_lacks_issuer`.
- `advice_extension_audience_cannot_override_assertion_conditions_audience`.
- `duplicate_id_attribute_triggers_signature_wrapping_detected`.
- `multi_audience_matches_second_entry_and_subject_confirmation_and_replay_cache_work`.
- `normalized_attribute_unescapes_recipient_entities`.
- `multiple_bearer_subject_confirmations_succeed_when_first_is_valid_and_second_is_invalid`.
- `multiple_audience_restriction_groups_require_and_across_groups`.
- `expected_in_response_to_does_not_fall_back_to_unsigned_response_attribute`.
- `replay_cache_retains_entry_through_clock_skew_window`.
- `typed_condition_audience_restriction_is_enforced_and_unknown_condition_fails_closed`.
- `subject_confirmation_requires_explicit_bearer_method_and_not_on_or_after_and_outer_in_response_to`.
- `xsi_type_qname_trims_xml_schema_whitespace_before_namespace_resolution`.
- `xsi_type_unicode_whitespace_prefix_is_not_trimmed_into_saml_prefix`.

## Verification

```
cargo build
cargo test -p rume-enterprise --features saml
cargo test -p rume --features enterprise
```

## Commit

Only when the user asks:

```
refactor(enterprise): move saml.rs into a saml/ module (assertion, verify, discovery)
```

## Done-when

- [ ] `crates/enterprise/src/saml/` contains `mod.rs`, `assertion.rs`, `verify.rs`, `discovery.rs`,
  `tests.rs`; `crates/enterprise/src/enterprise/saml.rs` deleted.
- [ ] `rume::enterprise::SamlParser` still resolves (enterprise/mod.rs re-export);
  `rume_enterprise::saml::SamlParser` resolves.
- [ ] `cargo build` + `cargo test -p rume-enterprise --features saml` + `cargo test -p rume --features enterprise` green.
