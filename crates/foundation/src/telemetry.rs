//! Shared telemetry primitives: request identity and pool status snapshots.

use std::fmt::{self, Display};
use std::ops::Deref;

use axum::extract::FromRequestParts;
use axum::http::HeaderName;
use axum::http::request::Parts;
use serde::{Deserialize, Serialize};

/// The standard HTTP header name for request identity: `x-request-id`.
pub static X_REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

/// Metrics and status information for a database connection pool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PoolStatus {
    /// Logical connection leases currently held by application callers via `acquire_lease`.
    pub leased_connections: u32,
    /// Physical active (busy) connections in the underlying driver pool (`total - idle`).
    pub active_connections: u32,
    /// Physical idle connections available in the underlying driver pool.
    pub idle_connections: u32,
    /// Physical total opened connections in the underlying driver pool.
    pub total_connections: u32,
    /// Configured maximum connection capacity.
    pub max_connections: u32,
    /// In-flight transaction handles.
    pub in_flight_transactions: u32,
    /// Whether the pool is closed.
    pub is_closed: bool,
}

/// A unique identifier for an HTTP request.
///
/// Propagated through request extensions and extractable in Axum handlers via
/// [`FromRequestParts`].
///
/// # Example
///
/// ```
/// use rume_foundation::RequestId;
///
/// async fn my_handler(request_id: RequestId) -> String {
///     format!("processing request {}", request_id.0)
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RequestId(pub String);

impl RequestId {
    /// Generate a new, unique UUID v4 formatted request ID backed by the OS CSPRNG.
    pub fn generate() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    /// Access the underlying string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Deref for RequestId {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AsRef<str> for RequestId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for RequestId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for RequestId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl<S> FromRequestParts<S> for RequestId
where
    S: Send + Sync,
{
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        if let Some(id) = parts.extensions.get::<RequestId>() {
            Ok(id.clone())
        } else {
            Ok(RequestId::generate())
        }
    }
}

/// Validate an incoming `X-Request-ID` value.
///
/// Ensures the value is non-empty, at most 128 characters, and contains only
/// safe ASCII characters (alphanumeric, `-`, `_`, `.`, `/`).
pub fn is_valid_request_id(value: &str) -> bool {
    let len = value.len();
    if !(1..=128).contains(&len) {
        return false;
    }
    value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' || c == '/')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_id_generate_produces_distinct_valid_ids() {
        let id1 = RequestId::generate();
        let id2 = RequestId::generate();
        assert_ne!(id1, id2);
        assert!(is_valid_request_id(id1.as_str()));
        assert!(is_valid_request_id(id2.as_str()));
    }

    #[test]
    fn is_valid_request_id_enforces_bounds_and_charset() {
        assert!(!is_valid_request_id(""));
        assert!(!is_valid_request_id(&"a".repeat(129)));
        assert!(!is_valid_request_id("bad\nid"));
        assert!(is_valid_request_id("req-123_abc.def/1"));
    }

    #[test]
    fn pool_status_serializes_expected_fields() {
        let status = PoolStatus {
            leased_connections: 2,
            active_connections: 1,
            idle_connections: 4,
            total_connections: 5,
            max_connections: 10,
            in_flight_transactions: 1,
            is_closed: false,
        };
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(json["leased_connections"], 2);
        assert_eq!(json["max_connections"], 10);
        assert_eq!(json["is_closed"], false);
    }
}
