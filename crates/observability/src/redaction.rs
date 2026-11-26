//! Structured, configurable HTTP header and bounded JSON body PII redaction.

use std::collections::BTreeMap;

use axum::http::HeaderMap;

/// Default replacement placeholder emitted for redacted values.
pub const DEFAULT_REDACTED_PLACEHOLDER: &str = "[REDACTED]";

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
#[derive(Debug, Clone, Default)]
pub struct RedactionConfig {}

impl RedactionConfig {
    /// Creates a redaction configuration with default sensitive headers and JSON keys.
    #[allow(unused_variables, dead_code)] // Removed in Task 04.
    pub fn new() -> Self {
        todo!("implemented in Task 04")
    }

    /// Returns a deterministic map of headers with configured sensitive values redacted.
    #[allow(unused_variables, dead_code)] // Removed in Task 04.
    pub fn redact_headers(&self, headers: &HeaderMap) -> BTreeMap<String, Vec<String>> {
        todo!("implemented in Task 04")
    }

    /// Recursively redacts sensitive keys in a JSON value up to the configured depth cap.
    #[allow(unused_variables, dead_code)] // Removed in Task 04.
    pub fn redact_json_value(&self, value: &mut serde_json::Value) {
        todo!("implemented in Task 04")
    }

    /// Parses and redacts a bounded JSON byte slice, returning `None` if oversized or invalid.
    #[allow(unused_variables, dead_code)] // Removed in Task 04.
    pub fn redact_json_bytes(&self, bytes: &[u8]) -> Option<String> {
        todo!("implemented in Task 04")
    }
}
