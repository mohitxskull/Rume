//! Structured, configurable HTTP header and bounded JSON body PII redaction.

use std::collections::{BTreeMap, BTreeSet};

use axum::http::HeaderMap;

/// Default replacement placeholder emitted for redacted values.
pub const DEFAULT_REDACTED_PLACEHOLDER: &str = "[REDACTED]";

/// Default maximum JSON nesting depth inspected before failing closed.
pub const DEFAULT_MAX_JSON_DEPTH: usize = 32;

/// Default maximum body size in bytes accepted by [`RedactionConfig::redact_json_bytes`] (64 KiB).
pub const DEFAULT_MAX_BODY_BYTES: usize = 64 * 1024;

/// Default HTTP header names (lowercase) redacted when header logging is enabled.
pub const DEFAULT_SENSITIVE_HEADERS: &[&str] = &[
    "authorization",
    "proxy-authorization",
    "cookie",
    "set-cookie",
    "x-api-key",
    "x-csrf-token",
];

/// Default JSON object keys (lowercase) redacted by [`RedactionConfig::redact_json_value`].
pub const DEFAULT_SENSITIVE_JSON_KEYS: &[&str] = &[
    "password",
    "passwd",
    "secret",
    "token",
    "access_token",
    "refresh_token",
    "id_token",
    "api_key",
    "apikey",
    "authorization",
    "credit_card",
    "card_number",
    "cvv",
    "ssn",
    "private_key",
];

/// Configuration for HTTP header and bounded JSON body PII redaction.
#[derive(Debug, Clone)]
pub struct RedactionConfig {
    sensitive_headers: BTreeSet<String>,
    sensitive_json_keys: BTreeSet<String>,
    placeholder: String,
    max_json_depth: usize,
    max_body_bytes: usize,
    log_headers: bool,
}

impl Default for RedactionConfig {
    fn default() -> Self {
        Self::new()
    }
}

impl RedactionConfig {
    /// Creates a redaction configuration with default sensitive headers and JSON keys.
    pub fn new() -> Self {
        Self {
            sensitive_headers: DEFAULT_SENSITIVE_HEADERS
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
            sensitive_json_keys: DEFAULT_SENSITIVE_JSON_KEYS
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
            placeholder: DEFAULT_REDACTED_PLACEHOLDER.to_owned(),
            max_json_depth: DEFAULT_MAX_JSON_DEPTH,
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            log_headers: false,
        }
    }

    /// Adds a single sensitive HTTP header name (matched case-insensitively).
    pub fn with_sensitive_header(mut self, header: impl Into<String>) -> Self {
        self.sensitive_headers
            .insert(header.into().to_ascii_lowercase());
        self
    }

    /// Adds multiple sensitive HTTP header names (matched case-insensitively).
    pub fn with_sensitive_headers<I, S>(mut self, headers: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.sensitive_headers
            .extend(headers.into_iter().map(|h| h.into().to_ascii_lowercase()));
        self
    }

    /// Adds a single sensitive JSON object key (matched case-insensitively).
    pub fn with_sensitive_json_key(mut self, key: impl Into<String>) -> Self {
        self.sensitive_json_keys.insert(key.into().to_lowercase());
        self
    }

    /// Adds multiple sensitive JSON object keys (matched case-insensitively).
    pub fn with_sensitive_json_keys<I, S>(mut self, keys: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.sensitive_json_keys
            .extend(keys.into_iter().map(|k| k.into().to_lowercase()));
        self
    }

    /// Sets the replacement placeholder emitted for redacted values.
    pub fn with_placeholder(mut self, placeholder: impl Into<String>) -> Self {
        self.placeholder = placeholder.into();
        self
    }

    /// Sets the maximum JSON nesting depth inspected before failing closed.
    pub fn with_max_json_depth(mut self, max_depth: usize) -> Self {
        self.max_json_depth = max_depth;
        self
    }

    /// Sets the maximum body byte length accepted by [`Self::redact_json_bytes`].
    pub fn with_max_body_bytes(mut self, max_bytes: usize) -> Self {
        self.max_body_bytes = max_bytes;
        self
    }

    /// Enables or disables redacted header debug logging in `ObservabilityLayer`.
    pub fn with_log_headers(mut self, enabled: bool) -> Self {
        self.log_headers = enabled;
        self
    }

    /// Returns whether redacted header debug logging is enabled.
    pub fn log_headers(&self) -> bool {
        self.log_headers
    }

    /// Returns a deterministic map of headers with configured sensitive values redacted.
    ///
    /// Sensitive header values are always replaced with the configured placeholder, including
    /// non-UTF-8 header values. Non-sensitive headers are preserved (with lossy UTF-8 decoding
    /// if a header value contains non-UTF-8 bytes).
    pub fn redact_headers(&self, headers: &HeaderMap) -> BTreeMap<String, Vec<String>> {
        let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (name, value) in headers {
            let key = name.as_str();
            let rendered = if self.sensitive_headers.contains(key) {
                self.placeholder.clone()
            } else {
                value
                    .to_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|_| String::from_utf8_lossy(value.as_bytes()).into_owned())
            };
            out.entry(key.to_owned()).or_default().push(rendered);
        }
        out
    }

    /// Recursively redacts sensitive keys in a JSON value up to the configured depth cap.
    ///
    /// Object keys are matched case-insensitively. Any subtree whose nesting depth exceeds
    /// `max_json_depth` is replaced with the redaction placeholder (fail closed) so deeply
    /// nested payloads can neither leak uninspected PII nor trigger stack overflow.
    pub fn redact_json_value(&self, value: &mut serde_json::Value) {
        self.redact_json_value_at_depth(value, 0);
    }

    fn redact_json_value_at_depth(&self, value: &mut serde_json::Value, depth: usize) {
        // Why: fail closed once `depth` exceeds `max_json_depth` by replacing the uninspected
        // subtree with the placeholder. Leaving deep subtrees untouched would let deeply nested
        // JSON smuggle PII past the depth cap, while unbounded recursion risks stack overflow on
        // hostile inputs.
        if depth > self.max_json_depth {
            *value = serde_json::Value::String(self.placeholder.clone());
            return;
        }

        match value {
            serde_json::Value::Object(map) => {
                for (key, val) in map.iter_mut() {
                    if self.sensitive_json_keys.contains(&key.to_lowercase()) {
                        *val = serde_json::Value::String(self.placeholder.clone());
                    } else {
                        self.redact_json_value_at_depth(val, depth + 1);
                    }
                }
            }
            serde_json::Value::Array(items) => {
                for item in items.iter_mut() {
                    self.redact_json_value_at_depth(item, depth + 1);
                }
            }
            serde_json::Value::Null
            | serde_json::Value::Bool(_)
            | serde_json::Value::Number(_)
            | serde_json::Value::String(_) => {}
        }
    }

    /// Parses and redacts a bounded JSON byte slice, returning compact redacted JSON.
    ///
    /// Returns `None` if `bytes.len()` exceeds `max_body_bytes` or if `bytes` is not valid JSON.
    /// `None` is intentionally fail-closed: callers must **not** fall back to logging the raw
    /// bytes when `None` is returned.
    pub fn redact_json_bytes(&self, bytes: &[u8]) -> Option<String> {
        if bytes.len() > self.max_body_bytes {
            return None;
        }
        let mut value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
        self.redact_json_value(&mut value);
        serde_json::to_string(&value).ok()
    }
}

#[cfg(test)]
mod tests {
    use axum::http::header::{HeaderName, HeaderValue};
    use serde_json::json;

    use crate::redaction::{DEFAULT_REDACTED_PLACEHOLDER, RedactionConfig};

    #[test]
    fn redact_headers_masks_default_sensitive_headers_case_insensitively() {
        let cfg = RedactionConfig::new();
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            HeaderName::from_static("authorization"),
            HeaderValue::from_static("Bearer top-secret"),
        );
        headers.insert(
            HeaderName::from_static("cookie"),
            HeaderValue::from_static("session=abc123"),
        );
        headers.append(
            HeaderName::from_static("set-cookie"),
            HeaderValue::from_static("session=xyz789; HttpOnly"),
        );
        headers.append(
            HeaderName::from_static("set-cookie"),
            HeaderValue::from_static("csrf=tok456; Secure"),
        );
        headers.insert(
            HeaderName::from_static("x-api-key"),
            HeaderValue::from_static("sk-live-12345"),
        );
        headers.insert(
            HeaderName::from_static("proxy-authorization"),
            HeaderValue::from_bytes(b"Basic \xff\xfe").expect("valid non-utf8 header bytes"),
        );
        headers.insert(
            HeaderName::from_static("user-agent"),
            HeaderValue::from_static("rume-test/1.0"),
        );

        let redacted = cfg.redact_headers(&headers);

        assert_eq!(
            redacted.get("authorization"),
            Some(&vec![DEFAULT_REDACTED_PLACEHOLDER.to_string()])
        );
        assert_eq!(
            redacted.get("cookie"),
            Some(&vec![DEFAULT_REDACTED_PLACEHOLDER.to_string()])
        );
        assert_eq!(
            redacted.get("set-cookie"),
            Some(&vec![
                DEFAULT_REDACTED_PLACEHOLDER.to_string(),
                DEFAULT_REDACTED_PLACEHOLDER.to_string(),
            ])
        );
        assert_eq!(
            redacted.get("x-api-key"),
            Some(&vec![DEFAULT_REDACTED_PLACEHOLDER.to_string()])
        );
        assert_eq!(
            redacted.get("proxy-authorization"),
            Some(&vec![DEFAULT_REDACTED_PLACEHOLDER.to_string()])
        );
        assert_eq!(
            redacted.get("user-agent"),
            Some(&vec!["rume-test/1.0".to_string()])
        );
    }

    #[test]
    fn redact_headers_masks_custom_configured_headers() {
        let cfg = RedactionConfig::new()
            .with_sensitive_header("X-Internal-Secret")
            .with_sensitive_headers(["X-Extra-Token"]);
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            HeaderName::from_static("x-internal-secret"),
            HeaderValue::from_static("internal-only-value"),
        );
        headers.insert(
            HeaderName::from_static("x-extra-token"),
            HeaderValue::from_static("extra-secret"),
        );
        headers.insert(
            HeaderName::from_static("x-request-id"),
            HeaderValue::from_static("req-42"),
        );

        let redacted = cfg.redact_headers(&headers);

        assert_eq!(
            redacted.get("x-internal-secret"),
            Some(&vec![DEFAULT_REDACTED_PLACEHOLDER.to_string()])
        );
        assert_eq!(
            redacted.get("x-extra-token"),
            Some(&vec![DEFAULT_REDACTED_PLACEHOLDER.to_string()])
        );
        assert_eq!(
            redacted.get("x-request-id"),
            Some(&vec!["req-42".to_string()])
        );
    }

    #[test]
    fn redact_json_value_masks_nested_objects_and_arrays() {
        let cfg = RedactionConfig::new();
        let mut payload = json!({
            "username": "alice",
            "status": "active",
            "password": "hunter2",
            "Access_Token": "eyJhbGciOi...",
            "user": {
                "display_name": "Alice",
                "credentials": {
                    "api_key": "sk-secret-999",
                    "scope": "read"
                }
            },
            "sessions": [
                {
                    "id": "sess-1",
                    "refresh_token": "ref-abc"
                },
                {
                    "id": "sess-2",
                    "SECRET": { "nested": "value" }
                }
            ]
        });

        cfg.redact_json_value(&mut payload);

        assert_eq!(payload["username"], "alice");
        assert_eq!(payload["status"], "active");
        assert_eq!(payload["password"], DEFAULT_REDACTED_PLACEHOLDER);
        assert_eq!(payload["Access_Token"], DEFAULT_REDACTED_PLACEHOLDER);
        assert_eq!(payload["user"]["display_name"], "Alice");
        assert_eq!(
            payload["user"]["credentials"]["api_key"],
            DEFAULT_REDACTED_PLACEHOLDER
        );
        assert_eq!(payload["user"]["credentials"]["scope"], "read");
        assert_eq!(payload["sessions"][0]["id"], "sess-1");
        assert_eq!(
            payload["sessions"][0]["refresh_token"],
            DEFAULT_REDACTED_PLACEHOLDER
        );
        assert_eq!(payload["sessions"][1]["id"], "sess-2");
        assert_eq!(
            payload["sessions"][1]["SECRET"],
            DEFAULT_REDACTED_PLACEHOLDER
        );
    }

    #[test]
    fn redact_json_value_fails_closed_past_max_depth() {
        let cfg = RedactionConfig::new()
            .with_max_json_depth(3)
            .with_placeholder("***");

        // Build a 10-level nested JSON object with PII at the leaf.
        let mut value = json!({ "password": "deep-secret-value" });
        for level in 0..10 {
            value = json!({ format!("level_{level}"): value });
        }

        cfg.redact_json_value(&mut value);

        let serialized = serde_json::to_string(&value).expect("serialize redacted value");
        assert!(
            !serialized.contains("deep-secret-value"),
            "deep PII must never survive past max_json_depth"
        );
        assert!(
            serialized.contains("***"),
            "exceeded subtree must be replaced with configured placeholder"
        );
        // Depth 0 (level_9) -> depth 1 (level_8) -> depth 2 (level_7) -> depth 3 (level_6) -> depth 4 (> 3) redacted.
        assert_eq!(value["level_9"]["level_8"]["level_7"]["level_6"], "***");
    }

    #[test]
    fn redact_json_bytes_skips_oversized_or_non_json_payloads() {
        let cfg = RedactionConfig::new().with_max_body_bytes(64);

        // Oversized payload returns None (fail closed).
        let oversized = format!(r#"{{"password":"{}"}}"#, "x".repeat(100));
        assert!(oversized.len() > 64);
        assert_eq!(cfg.redact_json_bytes(oversized.as_bytes()), None);

        // Invalid JSON returns None (fail closed).
        assert_eq!(cfg.redact_json_bytes(b"not-valid-json"), None);

        // Valid bounded JSON returns compact redacted string.
        let valid = br#"{"username":"bob","password":"secret"}"#;
        let redacted = cfg
            .redact_json_bytes(valid)
            .expect("bounded valid JSON should be redacted");
        let parsed: serde_json::Value =
            serde_json::from_str(&redacted).expect("redacted output must be valid JSON");
        assert_eq!(parsed["username"], "bob");
        assert_eq!(parsed["password"], DEFAULT_REDACTED_PLACEHOLDER);
        assert!(!redacted.contains("secret"));
    }
}
