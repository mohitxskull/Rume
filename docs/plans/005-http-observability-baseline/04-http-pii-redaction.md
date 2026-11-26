# Task 04 — HTTP Header & Body PII Redaction

## Goal

Implement structured, configurable HTTP header and bounded JSON body PII redaction in `rume-observability::redaction`, and wire header redaction into `ObservabilityLayer` so sensitive request/response headers are never leaked in spans or logs without breaking streaming or adding unnecessary body-buffering overhead.

## Files touched

- `crates/observability/src/redaction.rs`
- `crates/observability/src/layer.rs`

## Deliverable

1. **Redaction engine (`crates/observability/src/redaction.rs`)**:
   - `RedactionConfig` builder:
     - Default sensitive headers (case-insensitive): `authorization`, `proxy-authorization`, `cookie`, `set-cookie`, `x-api-key`, `x-csrf-token`.
     - Default sensitive JSON keys (case-insensitive): `password`, `passwd`, `secret`, `token`, `access_token`, `refresh_token`, `id_token`, `api_key`, `apikey`, `authorization`, `credit_card`, `card_number`, `cvv`, `ssn`, `private_key`.
     - Builder methods: `with_sensitive_header(...)`, `with_sensitive_headers(...)`, `with_sensitive_json_key(...)`, `with_sensitive_json_keys(...)`, `with_placeholder(...)` (default `"[REDACTED]"`), `with_max_json_depth(usize)` (default `32`, preventing stack overflow on deeply nested hostile JSON), `with_max_body_bytes(usize)` (default `64 * 1024`, i.e. 64 KiB cap for opt-in body redaction), `with_log_headers(bool)` (default `false` so existing zero-overhead span shape is preserved unless opted in).
   - Pure redaction methods on `RedactionConfig`:
     - `RedactionConfig::redact_headers(&self, headers: &HeaderMap) -> BTreeMap<String, Vec<String>>` (or formatted deterministic redaction view) — replaces values of any configured sensitive header with the placeholder (including non-UTF-8 values), leaving non-sensitive headers intact.
     - `RedactionConfig::redact_json_value(&self, value: &mut serde_json::Value)` — recursively traverses objects/arrays up to `max_json_depth`, replacing values whose object key matches a sensitive key (case-insensitive) with `Value::String(placeholder)`. When depth exceeds `max_json_depth`, replaces the subtree with the placeholder (fail closed — never leaves deep uninspected PII unredacted).
     - `RedactionConfig::redact_json_bytes(&self, bytes: &[u8]) -> Option<String>` — returns `None` if `bytes.len() > max_body_bytes` or invalid JSON, otherwise parses, redacts via `redact_json_value`, and serializes compact JSON for safe logging. Document clearly in rustdoc that `None` is fail-closed ("do not log raw bytes").
2. **`ObservabilityLayer` integration (`crates/observability/src/layer.rs`)**:
   - Keep `ObservabilityLayer::new()` / `Default` 100% backwards compatible (zero extra allocations on the hot path when `log_headers` is `false`).
   - Add `ObservabilityLayer::with_redaction(config: RedactionConfig) -> Self` (stored behind `Arc<RedactionConfig>` so `ObservabilityService` clones remain `O(1)`).
   - When `config.log_headers` is enabled, emit redacted request-header `tracing::debug!` events **inside** the `async move { ... }.instrument(span)` block (right before `inner.call(req).await?`, not outside `Box::pin`) and redacted response-header `tracing::debug!` events right after `inner.call(req).await?` so both attach to the `http_request` span — guaranteeing `Authorization`, `Cookie`, `Set-Cookie`, and custom sensitive headers are always replaced by `[REDACTED]`.
   - **Never** buffer request or response bodies in `ObservabilityLayer` for redaction (body redaction remains an explicit, bounded helper via `RedactionConfig::redact_json_value` / `redact_json_bytes` so streaming endpoints and the `application/problem+json` body-integrity invariant at `crates/observability/src/layer.rs:221-313` are untouched).

## Constraints honoured

- `#![forbid(unsafe_code)]` (`crates/observability/src/lib.rs:3`).
- Preserve all existing `ObservabilityLayer` tests in `crates/observability/src/layer.rs:323-821` (W3C `traceparent`/`tracestate` propagation, baggage isolation, `MatchedPath` recording, oversized/invalid/erroring `application/problem+json` stream preservation).
- `rules/70-code-rationale.md`: document `Why:` on fail-closed `max_json_depth` truncation and why `ObservabilityLayer` does not unconditionally buffer bodies.
- `rules/85-minimalism.md`: zero new crate dependencies; reuse `serde_json` and `axum::http::HeaderMap` already in `rume-observability`.

## Named tests (bodies omitted)

In `crates/observability/src/redaction.rs`:
- `redact_headers_masks_default_sensitive_headers_case_insensitively` — passes `Authorization`, `cookie`, `Set-Cookie`, `X-API-Key`, and `User-Agent`; asserts all sensitive headers become `"[REDACTED]"` while `User-Agent` is preserved.
- `redact_headers_masks_custom_configured_headers` — adds `x-internal-secret`, verifies it is redacted while unrelated headers remain intact.
- `redact_json_value_masks_nested_objects_and_arrays` — redacts `password`, `Access_Token`, nested `user.credentials.api_key`, and elements inside arrays, leaving `username` and `status` untouched.
- `redact_json_value_fails_closed_past_max_depth` — constructs a nested JSON object deeper than `max_json_depth`, asserts the deep subtree is replaced with the placeholder without stack overflow.
- `redact_json_bytes_skips_oversized_or_non_json_payloads` — verifies payloads larger than `max_body_bytes` or invalid JSON return `None`.

In `crates/observability/src/layer.rs`:
- `observability_layer_with_redaction_never_logs_raw_authorization_or_cookie` — installs a custom capturing `tracing_subscriber` layer (using `set_default` + `test_global_otel_lock`), sends a request with `Authorization: Bearer top-secret-token` and `Cookie: session=abc123`, handler returns `Set-Cookie: session=xyz789`; asserts captured debug events contain `"[REDACTED]"` and do NOT contain `"top-secret-token"`, `"abc123"`, or `"xyz789"`.
- `observability_layer_with_redaction_preserves_streaming_response` — verifies a streaming response passes through `ObservabilityLayer::with_redaction(...)` untouched without buffering.

## Verification

```bash
cargo fmt --all --check
cargo clippy -j 2 -p rume-observability --all-targets --all-features -- -D warnings
cargo test -j 2 -p rume-observability --lib --all-features redaction::
cargo test -j 2 -p rume-observability --lib --all-features layer::
```

Expected: zero warnings; all new `redaction::` tests and all existing + new `layer::` tests pass.

## Commit

*Only when the user asks:*

```text
feat(observability): add HTTP header and bounded JSON PII redaction
```

## Done-when

- [ ] `RedactionConfig`, `redact_headers`, `redact_json_value`, and `redact_json_bytes` are implemented with zero `todo!()` stubs remaining.
- [ ] `ObservabilityLayer::with_redaction` redacts sensitive request/response headers without buffering streams or regressing any existing `layer::tests`.
- [ ] All 7 named tests pass and clippy is clean with zero warnings.
