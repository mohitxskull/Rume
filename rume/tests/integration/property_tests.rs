//! Property-based testing verification using proptest.
//!
//! Validates:
//! - W3C TraceContext roundtripping, invariants, and fuzz input resilience
//! - Diagnostic text redaction and secret sanitization on arbitrary UTF-8 inputs
//! - String helper truncation, masking, and whitespace invariant preservation

use proptest::prelude::*;
use rume::app::TraceContext;
use rume::env::secrets::redact_diagnostic_text;
use rume::helpers::mask::mask_secret;
use rume::helpers::truncate::truncate;
use rume::helpers::whitespace::condense_whitespace;

proptest! {
    // -------------------------------------------------------------------------
    // 1. W3C TraceContext Invariants & Fuzzing
    // -------------------------------------------------------------------------

    #[test]
    fn trace_context_roundtrips_arbitrary_valid_traces(
        trace_id in "[0-9a-f]{32}",
        span_id in "[0-9a-f]{16}",
        tracestate in proptest::option::of("[a-z0-9_-]{1,16}=[a-zA-Z0-9_-]{1,32}(,[a-z0-9_-]{1,16}=[a-zA-Z0-9_-]{1,32}){0,3}"),
    ) {
        // Exclude all-zero IDs per W3C specification
        prop_assume!(trace_id != "00000000000000000000000000000000");
        prop_assume!(span_id != "0000000000000000");

        let traceparent = format!("00-{trace_id}-{span_id}-01");
        prop_assert!(TraceContext::is_valid_traceparent(&traceparent));

        let ctx = TraceContext {
            traceparent: traceparent.clone(),
            tracestate: tracestate.clone(),
        };

        let serialized = ctx.to_serialized();
        let parsed = TraceContext::parse_serialized(&serialized);
        prop_assert!(parsed.is_some(), "must parse serialized tracecontext");
        let parsed = parsed.unwrap();

        prop_assert_eq!(&parsed.traceparent, &traceparent);
        prop_assert_eq!(&parsed.tracestate, &tracestate);
        prop_assert_eq!(parsed.trace_id(), trace_id.as_str());
        prop_assert_eq!(parsed.span_id(), span_id.as_str());

        // Child context preserves trace_id and tracestate
        let child = parsed.new_child();
        prop_assert_eq!(child.trace_id(), trace_id.as_str());
        prop_assert_ne!(child.span_id(), span_id.as_str());
        prop_assert_eq!(&child.tracestate, &tracestate);
    }

    #[test]
    fn trace_context_never_panics_on_arbitrary_fuzz_strings(s in "\\PC*") {
        let is_valid = TraceContext::is_valid_traceparent(&s);
        let parsed = TraceContext::parse_serialized(&s);
        if is_valid {
            prop_assert!(parsed.is_some());
        }
    }

    // -------------------------------------------------------------------------
    // 2. Secret Redaction Invariants on Arbitrary Inputs
    // -------------------------------------------------------------------------

    #[test]
    fn redact_diagnostic_text_never_panics_and_removes_known_secret(
        secret in "[a-zA-Z0-9!@#$%^&*()_+]{8,32}",
        prefix in "\\PC{0,64}",
        suffix in "\\PC{0,64}",
    ) {
        let raw_message = format!("{prefix}{secret}{suffix}");
        let redacted = redact_diagnostic_text(&raw_message, &[&secret]);

        if !secret.is_empty() {
            prop_assert!(
                !redacted.contains(&secret),
                "redacted output must not contain secret '{secret}': {redacted}"
            );
        }
    }

    // -------------------------------------------------------------------------
    // 3. String Manipulation Helpers Invariants
    // -------------------------------------------------------------------------

    #[test]
    fn truncate_respects_max_length_bound(
        s in "\\PC*",
        max_len in 0usize..256,
    ) {
        let result = truncate(&s, max_len);
        let char_count = result.chars().count();
        prop_assert!(
            char_count <= max_len.max(3),
            "truncated char count {} exceeded max_len {}",
            char_count,
            max_len
        );
    }

    #[test]
    fn mask_secret_preserves_edges_and_abbreviates_middle(
        s in "\\PC*",
    ) {
        let masked = mask_secret(&s);
        let s_chars = s.chars().count();

        if s_chars <= 6 {
            prop_assert_eq!(masked, "******");
        } else {
            prop_assert!(masked.contains("..."));
            let edge_len = if s_chars <= 12 { 2 } else { 4 };
            let prefix: String = s.chars().take(edge_len).collect();
            let suffix: String = s.chars().skip(s_chars - edge_len).collect();
            prop_assert!(masked.starts_with(&prefix));
            prop_assert!(masked.ends_with(&suffix));
        }
    }

    #[test]
    fn condense_whitespace_eliminates_adjacent_spaces(
        s in "\\PC*",
    ) {
        let condensed = condense_whitespace(&s);
        prop_assert!(!condensed.contains("  "), "condensed string contains double spaces: '{condensed}'");
        prop_assert!(!condensed.contains(" \t"), "condensed string contains space-tab");
        prop_assert!(!condensed.contains("\t "), "condensed string contains tab-space");
    }
}
