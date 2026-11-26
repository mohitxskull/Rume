//! Outbound webhook delivery with HMAC signing for Rume.

pub mod dispatcher;
pub mod signer;
pub mod store;

pub use dispatcher::{
    ReqwestWebhookTransport, WebhookDeliveryJob, WebhookDeliveryJobHandler, WebhookDispatcher,
    WebhookHttpResponse, WebhookTransport,
};
pub use signer::sign_webhook_payload;
pub use store::{
    InMemoryWebhookDeliveryStore, SqlWebhookDeliveryStore, WebhookDeliveryRecord,
    WebhookDeliveryStatus, WebhookDeliveryStore,
};

use secrecy::SecretString;
use thiserror::Error;

/// Errors returned by outbound webhook signing, storage, or delivery.
#[derive(Debug, Error)]
pub enum WebhookError {
    /// Failed to serialize or deserialize a webhook payload.
    #[error("webhook serialization error: {0}")]
    Serialization(String),
    /// Failed to persist or query a webhook delivery record.
    #[error("webhook store error: {0}")]
    Store(String),
    /// HTTP transport failure while delivering a webhook.
    #[error("webhook transport error: {0}")]
    Transport(String),
    /// Requested webhook endpoint was not registered.
    #[error("webhook endpoint not found: {0}")]
    EndpointNotFound(String),
}

/// Configuration for an outbound webhook subscription endpoint.
#[derive(Debug, Clone)]
pub struct WebhookEndpoint {
    /// Unique identifier for this webhook endpoint.
    pub id: String,
    /// Optional tenant scope for multi-tenant webhook isolation.
    pub tenant_id: Option<String>,
    /// Destination HTTPS/HTTP URL to receive webhook POST requests.
    pub url: String,
    /// Shared HMAC-SHA256 signing secret.
    pub secret: SecretString,
    /// Event types subscribed to (empty or `"*"` matches all events).
    pub events: Vec<String>,
    /// Whether this endpoint is currently active.
    pub active: bool,
}

impl WebhookEndpoint {
    /// Creates a new active [`WebhookEndpoint`] subscribed to all events.
    pub fn new(id: impl Into<String>, url: impl Into<String>, secret: SecretString) -> Self {
        Self {
            id: id.into(),
            tenant_id: None,
            url: url.into(),
            secret,
            events: Vec::new(),
            active: true,
        }
    }

    /// Scopes the endpoint to a specific tenant.
    pub fn with_tenant_id(mut self, tenant_id: impl Into<String>) -> Self {
        self.tenant_id = Some(tenant_id.into());
        self
    }

    /// Restricts delivery to the specified event types.
    pub fn with_events<I, S>(mut self, events: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.events = events.into_iter().map(Into::into).collect();
        self
    }

    /// Sets whether this endpoint is active.
    pub fn with_active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }

    /// Returns `true` if this endpoint is active and subscribed to `event_type`.
    pub fn subscribes_to(&self, event_type: &str) -> bool {
        self.active
            && (self.events.is_empty() || self.events.iter().any(|e| e == "*" || e == event_type))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use secrecy::SecretString;

    use super::*;
    use crate::jobs::RetryPolicy;
    use crate::outbox::repository::BoxFuture;

    #[derive(Debug, Clone)]
    struct RecordedRequest {
        url: String,
        signature: String,
        event_type: String,
        delivery_id: String,
        body: Vec<u8>,
    }

    #[derive(Debug)]
    struct MockTransport {
        responses: Mutex<Vec<Result<WebhookHttpResponse, WebhookError>>>,
        requests: Mutex<Vec<RecordedRequest>>,
    }

    impl MockTransport {
        fn new(responses: Vec<Result<WebhookHttpResponse, WebhookError>>) -> Self {
            Self {
                responses: Mutex::new(responses),
                requests: Mutex::new(Vec::new()),
            }
        }

        fn requests(&self) -> Vec<RecordedRequest> {
            self.requests.lock().unwrap().clone()
        }
    }

    impl WebhookTransport for MockTransport {
        fn send<'a>(
            &'a self,
            url: &'a str,
            signature_header: &'a str,
            event_type: &'a str,
            delivery_id: &'a str,
            body: Vec<u8>,
        ) -> BoxFuture<'a, Result<WebhookHttpResponse, WebhookError>> {
            let req = RecordedRequest {
                url: url.to_string(),
                signature: signature_header.to_string(),
                event_type: event_type.to_string(),
                delivery_id: delivery_id.to_string(),
                body,
            };
            self.requests.lock().unwrap().push(req);
            let next = self.responses.lock().unwrap().remove(0);
            Box::pin(async move { next })
        }
    }

    #[tokio::test]
    async fn outbound_webhook_dispatcher_signs_and_logs_delivery_with_backoff() {
        let store = Arc::new(InMemoryWebhookDeliveryStore::new());
        let transport = Arc::new(MockTransport::new(vec![
            Ok(WebhookHttpResponse {
                status: 503,
                body: Some("unavailable".into()),
            }),
            Ok(WebhookHttpResponse {
                status: 200,
                body: Some("ok".into()),
            }),
        ]));

        let dispatcher = WebhookDispatcher::new(store.clone(), transport.clone())
            .with_retry_policy(
                RetryPolicy::new()
                    .with_max_attempts(3)
                    .with_initial_backoff(Duration::from_millis(1))
                    .with_max_backoff(Duration::from_millis(50)),
            );

        let endpoint = WebhookEndpoint::new(
            "ep-1",
            "https://example.test/webhook",
            SecretString::from("whsec_test_secret".to_string()),
        )
        .with_events(["order.paid"]);
        dispatcher.register_endpoint(endpoint);

        let records = dispatcher
            .dispatch_event("order.paid", serde_json::json!({"order_id": "ord-123"}))
            .await
            .expect("dispatch_event");
        assert_eq!(records.len(), 1);
        let delivery_id = records[0].id.clone();

        let after_first = store
            .get(&delivery_id)
            .await
            .expect("get")
            .expect("delivery exists");
        assert_eq!(after_first.status, WebhookDeliveryStatus::Pending);
        assert_eq!(after_first.attempt, 1);
        assert!(after_first.next_attempt_at.is_some());
        assert_eq!(after_first.last_response.as_deref(), Some("HTTP 503"));

        tokio::time::sleep(Duration::from_millis(10)).await;
        let retried = dispatcher
            .process_due_deliveries(10)
            .await
            .expect("process_due");
        assert_eq!(retried, 1);

        let after_second = store
            .get(&delivery_id)
            .await
            .expect("get")
            .expect("delivery exists");
        assert_eq!(after_second.status, WebhookDeliveryStatus::Delivered);
        assert_eq!(after_second.attempt, 2);
        assert!(after_second.next_attempt_at.is_none());
        assert_eq!(after_second.last_response.as_deref(), Some("HTTP 200"));

        let reqs = transport.requests();
        assert_eq!(reqs.len(), 2);
        assert_eq!(reqs[0].url, "https://example.test/webhook");
        assert_eq!(reqs[0].event_type, "order.paid");
        assert_eq!(reqs[0].delivery_id, delivery_id);
        assert!(reqs[0].signature.starts_with("t="));
        assert!(reqs[0].signature.contains(",v1="));
        assert_eq!(reqs[0].signature.split(",v1=").nth(1).unwrap().len(), 64);
        assert!(!reqs[0].body.is_empty());
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn sql_webhook_delivery_store_persists_and_queries_due_deliveries() {
        let backend = Arc::new(
            rume_data::persistence::SqliteBackend::in_memory()
                .await
                .expect("sqlite in-memory"),
        );
        let store = SqlWebhookDeliveryStore::new(backend);
        store.ensure_schema().await.expect("ensure_schema");

        let now = chrono::Utc::now();
        let record = WebhookDeliveryRecord {
            id: "del-1".into(),
            endpoint_id: "ep-sql".into(),
            event_type: "user.created".into(),
            payload: r#"{"id":"u-1"}"#.into(),
            status: WebhookDeliveryStatus::Pending,
            attempt: 1,
            next_attempt_at: Some(now - chrono::Duration::seconds(5)),
            last_response: Some("HTTP 502".into()),
            created_at: now,
            updated_at: now,
        };
        store.upsert(record.clone()).await.expect("upsert");

        let fetched = store.get("del-1").await.expect("get").expect("present");
        assert_eq!(fetched.endpoint_id, "ep-sql");
        assert_eq!(fetched.status, WebhookDeliveryStatus::Pending);

        let due = store.list_due(now, 10).await.expect("list_due");
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].id, "del-1");
    }
}
