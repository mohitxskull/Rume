//! Outbox message model and state representation.

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::events::Event;
use crate::outbox::error::OutboxError;

pub(crate) fn current_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Lifecycle status of an outbox message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutboxStatus {
    /// Message is waiting to be published by the relay.
    Pending,
    /// Message has been claimed by a relay worker and is actively in flight.
    InFlight,
    /// Message was successfully published to the event bus or broker.
    Published,
    /// Message failed to publish and exceeded retry thresholds.
    Failed,
}

/// A durable outbox message stored atomically within a database transaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxMessage {
    /// Unique identifier for this outbox entry.
    pub id: String,
    /// Canonical event type identifier.
    pub event_type: String,
    /// Serialized JSON event payload.
    pub payload: Vec<u8>,
    /// Current publication status.
    pub status: OutboxStatus,
    /// Number of relay delivery attempts.
    pub retry_count: u32,
    /// Timestamp when this message was enqueued.
    pub created_at: u64,
    /// Diagnostic error message if the last delivery attempt failed.
    pub last_error: Option<String>,
    /// Earliest timestamp before which this message should not be retried.
    pub next_retry_at: Option<u64>,
    /// Unique token identifying the relay worker instance holding the claim lease.
    pub lease_token: Option<String>,
    /// Timestamp when the worker's exclusive claim lease expires.
    pub lease_expires_at: Option<u64>,
    /// Optional W3C TraceContext traceparent string propagating distributed trace identity.
    pub trace_context: Option<String>,
}

impl OutboxMessage {
    /// Creates an outbox message from raw ID, event type, and payload.
    pub fn new(id: impl Into<String>, event_type: impl Into<String>, payload: Vec<u8>) -> Self {
        Self {
            id: id.into(),
            event_type: event_type.into(),
            payload,
            status: OutboxStatus::Pending,
            retry_count: 0,
            created_at: current_timestamp(),
            last_error: None,
            next_retry_at: None,
            lease_token: None,
            lease_expires_at: None,
            trace_context: None,
        }
    }

    /// Attaches a W3C trace context string to the message.
    pub fn with_trace_context(mut self, trace_context: impl Into<String>) -> Self {
        self.trace_context = Some(trace_context.into());
        self
    }

    /// Creates a new outbox message from a serializable event.
    pub fn from_event<E>(event: &E) -> Result<Self, OutboxError>
    where
        E: Event + Serialize,
    {
        let payload =
            serde_json::to_vec(event).map_err(|e| OutboxError::Serialization(e.to_string()))?;

        Ok(Self {
            id: Uuid::new_v4().to_string(),
            event_type: E::TYPE.to_string(),
            payload,
            status: OutboxStatus::Pending,
            retry_count: 0,
            created_at: current_timestamp(),
            last_error: None,
            next_retry_at: None,
            lease_token: None,
            lease_expires_at: None,
            trace_context: None,
        })
    }

    /// Deserializes the stored payload into a concrete typed event.
    pub fn deserialize_payload<E: serde::de::DeserializeOwned>(&self) -> Result<E, OutboxError> {
        serde_json::from_slice(&self.payload).map_err(|e| OutboxError::Serialization(e.to_string()))
    }
}
