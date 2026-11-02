//! Structured audit logging for compliance, security events, and actor activity tracking.

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

/// Status outcome of an audited event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuditStatus {
    /// Action succeeded.
    Success,
    /// Action failed due to validation or server error.
    Failure,
    /// Action was denied due to missing permissions or authentication.
    Denied,
}

/// An immutable, structured audit log entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditRecord {
    /// Unique record ID.
    pub id: String,
    /// Epoch timestamp in milliseconds.
    pub timestamp_ms: u64,
    /// Identifier of the actor executing the action (if authenticated).
    pub actor_id: Option<String>,
    /// Action performed (e.g. "auth.login", "user.create", "settings.update").
    pub action: String,
    /// Target entity type (e.g. "user", "organization", "api_key").
    pub target_type: String,
    /// Specific target identifier.
    pub target_id: Option<String>,
    /// Client IP address.
    pub ip_address: Option<String>,
    /// Outcome status.
    pub status: AuditStatus,
    /// Structured contextual metadata.
    pub metadata: BTreeMap<String, String>,
}

impl AuditRecord {
    /// Creates a new audit log record with an automatic UUID v4 and current timestamp.
    pub fn new(
        action: impl Into<String>,
        target_type: impl Into<String>,
        status: AuditStatus,
    ) -> Self {
        let timestamp_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        Self {
            id: Uuid::new_v4().to_string(),
            timestamp_ms,
            actor_id: None,
            action: action.into(),
            target_type: target_type.into(),
            target_id: None,
            ip_address: None,
            status,
            metadata: BTreeMap::new(),
        }
    }

    /// Attaches an actor ID.
    pub fn with_actor(mut self, actor_id: impl Into<String>) -> Self {
        self.actor_id = Some(actor_id.into());
        self
    }

    /// Attaches a target ID.
    pub fn with_target(mut self, target_id: impl Into<String>) -> Self {
        self.target_id = Some(target_id.into());
        self
    }

    /// Attaches a client IP address.
    pub fn with_ip(mut self, ip: impl Into<String>) -> Self {
        self.ip_address = Some(ip.into());
        self
    }

    /// Attaches metadata key-value pair.
    pub fn with_meta(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }
}

/// Errors occurring during audit log recording.
#[derive(Debug, Error)]
pub enum AuditError {
    /// Underlying storage or sink failure.
    #[error("audit sink error: {0}")]
    Sink(String),
}

/// Boxed future alias for async audit logging.
pub type AuditFuture<'a> = Pin<Box<dyn Future<Output = Result<(), AuditError>> + Send + 'a>>;

/// Trait implemented by audit log sinks (in-memory, database, syslog, Kafka).
pub trait AuditLogger: Debug + Send + Sync + 'static {
    /// Records an audit log event.
    fn record<'a>(&'a self, record: AuditRecord) -> AuditFuture<'a>;
}

/// Thread-safe in-memory audit logger for testing and development.
#[derive(Debug, Clone, Default)]
pub struct MemoryAuditLogger {
    records: Arc<RwLock<Vec<AuditRecord>>>,
}

impl MemoryAuditLogger {
    /// Creates an empty in-memory audit store.
    pub fn new() -> Self {
        Self {
            records: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Returns a copy of all recorded audit events.
    pub fn records(&self) -> Vec<AuditRecord> {
        self.records.read().map(|r| r.clone()).unwrap_or_default()
    }

    /// Clears all recorded events.
    pub fn clear(&self) {
        if let Ok(mut r) = self.records.write() {
            r.clear();
        }
    }
}

impl AuditLogger for MemoryAuditLogger {
    fn record<'a>(&'a self, record: AuditRecord) -> AuditFuture<'a> {
        Box::pin(async move {
            let mut list = self
                .records
                .write()
                .map_err(|e| AuditError::Sink(e.to_string()))?;
            list.push(record);
            Ok(())
        })
    }
}
