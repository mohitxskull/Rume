//! Structured audit logging for compliance, security events, and actor activity tracking.

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;
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

use crate::persistence::backend::DatabaseBackend;

/// Trait implemented by audit log sinks (database, syslog, Kafka).
pub trait AuditLogger: Debug + Send + Sync + 'static {
    /// Records an audit log event.
    fn record<'a>(&'a self, record: AuditRecord) -> AuditFuture<'a>;
}

/// Persistent audit logger storing events in a relational database table via any [`DatabaseBackend`].
#[derive(Debug, Clone)]
pub struct SqlAuditLogger<B: DatabaseBackend> {
    backend: B,
}

impl<B: DatabaseBackend> SqlAuditLogger<B> {
    /// Schema DDL for audit log table.
    pub const SCHEMA: &'static str = r#"
CREATE TABLE IF NOT EXISTS rume_audit_log (
    id VARCHAR(64) PRIMARY KEY,
    action VARCHAR(255) NOT NULL,
    resource VARCHAR(255) NOT NULL,
    status VARCHAR(32) NOT NULL,
    actor_id VARCHAR(255),
    target_id VARCHAR(255),
    ip_address VARCHAR(64),
    metadata TEXT,
    created_at BIGINT NOT NULL
);
"#;

    /// Creates a new SQL audit logger.
    pub fn new(backend: B) -> Self {
        Self { backend }
    }

    /// Initializes the audit log database table.
    pub async fn init_table(&self) -> Result<(), AuditError> {
        self.backend
            .execute(Self::SCHEMA)
            .await
            .map_err(|e| AuditError::Sink(e.to_string()))?;
        Ok(())
    }

    /// Returns the underlying database backend.
    pub fn backend(&self) -> &B {
        &self.backend
    }

    /// Queries the total number of recorded audit logs.
    pub async fn count(&self) -> Result<usize, AuditError> {
        let count_str = self
            .backend
            .query_scalar("SELECT count(*) FROM rume_audit_log")
            .await
            .map_err(|e| AuditError::Sink(e.to_string()))?
            .unwrap_or_else(|| "0".to_string());
        count_str.parse().map_err(|e| AuditError::Sink(format!("{e}")))
    }
}

impl<B: DatabaseBackend> AuditLogger for SqlAuditLogger<B> {
    fn record<'a>(&'a self, record: AuditRecord) -> AuditFuture<'a> {
        Box::pin(async move {
            let id = uuid::Uuid::new_v4().to_string();
            let status_str = match record.status {
                AuditStatus::Success => "success",
                AuditStatus::Denied => "denied",
                AuditStatus::Failure => "failure",
            };
            let meta_json = serde_json::to_string(&record.metadata)
                .map_err(|e| AuditError::Sink(e.to_string()))?;
            let actor = record.actor_id.as_deref().unwrap_or("NULL");
            let target = record.target_id.as_deref().unwrap_or("NULL");
            let ip = record.ip_address.as_deref().unwrap_or("NULL");

            let actor_val = if actor == "NULL" { "NULL".to_string() } else { format!("'{}'", actor.replace('\'', "''")) };
            let target_val = if target == "NULL" { "NULL".to_string() } else { format!("'{}'", target.replace('\'', "''")) };
            let ip_val = if ip == "NULL" { "NULL".to_string() } else { format!("'{}'", ip.replace('\'', "''")) };
            let meta_val = format!("'{}'", meta_json.replace('\'', "''"));

            let stmt = format!(
                "INSERT INTO rume_audit_log (id, action, resource, status, actor_id, target_id, ip_address, metadata, created_at) VALUES ('{id}', '{}', '{}', '{status_str}', {actor_val}, {target_val}, {ip_val}, {meta_val}, {})",
                record.action.replace('\'', "''"),
                record.target_type.replace('\'', "''"),
                record.timestamp_ms
            );

            self.backend
                .execute(&stmt)
                .await
                .map_err(|e| AuditError::Sink(e.to_string()))?;
            Ok(())
        })
    }
}
