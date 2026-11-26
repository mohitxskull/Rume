//! Outbound webhook dispatcher, HTTP transport, and Outbox/JobQueue integrations.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::signer::sign_webhook_payload;
use super::store::{WebhookDeliveryRecord, WebhookDeliveryStatus, WebhookDeliveryStore};
use super::{WebhookEndpoint, WebhookError};
use crate::jobs::{Job, JobContext, JobError, JobHandler, RetryPolicy};
use crate::outbox::error::OutboxError;
use crate::outbox::message::OutboxMessage;
use crate::outbox::repository::BoxFuture;

/// HTTP response returned by a [`WebhookTransport`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookHttpResponse {
    /// HTTP status code returned by the webhook receiver.
    pub status: u16,
    /// Optional response body snippet.
    pub body: Option<String>,
}

/// Transport abstraction for sending signed outbound webhook HTTP requests.
pub trait WebhookTransport: std::fmt::Debug + Send + Sync + 'static {
    /// Sends a signed POST request to `url` with `X-Rume-Signature` and metadata headers.
    fn send<'a>(
        &'a self,
        url: &'a str,
        signature_header: &'a str,
        event_type: &'a str,
        delivery_id: &'a str,
        body: Vec<u8>,
    ) -> BoxFuture<'a, Result<WebhookHttpResponse, WebhookError>>;
}

/// Default [`WebhookTransport`] backed by `reqwest::Client`.
#[derive(Debug, Clone)]
pub struct ReqwestWebhookTransport {
    client: reqwest::Client,
}

impl Default for ReqwestWebhookTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl ReqwestWebhookTransport {
    /// Creates a new [`ReqwestWebhookTransport`] with a default `reqwest::Client`.
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }

    /// Creates a new [`ReqwestWebhookTransport`] using the supplied `reqwest::Client`.
    pub fn with_client(client: reqwest::Client) -> Self {
        Self { client }
    }
}

impl WebhookTransport for ReqwestWebhookTransport {
    fn send<'a>(
        &'a self,
        url: &'a str,
        signature_header: &'a str,
        event_type: &'a str,
        delivery_id: &'a str,
        body: Vec<u8>,
    ) -> BoxFuture<'a, Result<WebhookHttpResponse, WebhookError>> {
        Box::pin(async move {
            let response = self
                .client
                .post(url)
                .header("Content-Type", "application/json")
                .header("X-Rume-Signature", signature_header)
                .header("X-Rume-Event", event_type)
                .header("X-Rume-Delivery", delivery_id)
                .body(body)
                .send()
                .await
                .map_err(|e| WebhookError::Transport(e.to_string()))?;

            let status = response.status().as_u16();
            let text = response.text().await.ok();
            Ok(WebhookHttpResponse { status, body: text })
        })
    }
}

/// Dispatches signed outbound webhooks with exponential backoff and delivery logging.
#[derive(Debug, Clone)]
pub struct WebhookDispatcher {
    endpoints: Arc<RwLock<HashMap<String, WebhookEndpoint>>>,
    store: Arc<dyn WebhookDeliveryStore>,
    transport: Arc<dyn WebhookTransport>,
    retry_policy: RetryPolicy,
}

impl WebhookDispatcher {
    /// Creates a new [`WebhookDispatcher`].
    pub fn new(store: Arc<dyn WebhookDeliveryStore>, transport: Arc<dyn WebhookTransport>) -> Self {
        Self {
            endpoints: Arc::new(RwLock::new(HashMap::new())),
            store,
            transport,
            retry_policy: RetryPolicy::default(),
        }
    }

    /// Configures the [`RetryPolicy`] used for failed or non-2xx deliveries.
    pub fn with_retry_policy(mut self, policy: RetryPolicy) -> Self {
        self.retry_policy = policy;
        self
    }

    /// Registers an outbound webhook endpoint.
    pub fn register_endpoint(&self, endpoint: WebhookEndpoint) {
        if let Ok(mut guard) = self.endpoints.write() {
            guard.insert(endpoint.id.clone(), endpoint);
        }
    }

    fn matching_endpoints(&self, event_type: &str) -> Vec<WebhookEndpoint> {
        let Ok(guard) = self.endpoints.read() else {
            return Vec::new();
        };
        guard
            .values()
            .filter(|ep| ep.subscribes_to(event_type))
            .cloned()
            .collect()
    }

    fn endpoint_by_id(&self, id: &str) -> Option<WebhookEndpoint> {
        self.endpoints.read().ok()?.get(id).cloned()
    }

    /// Dispatches `event_type` with `payload` to all active matching endpoints.
    pub async fn dispatch_event(
        &self,
        event_type: &str,
        payload: serde_json::Value,
    ) -> Result<Vec<WebhookDeliveryRecord>, WebhookError> {
        let payload_str = serde_json::to_string(&payload)
            .map_err(|e| WebhookError::Serialization(e.to_string()))?;
        let endpoints = self.matching_endpoints(event_type);
        let mut results = Vec::with_capacity(endpoints.len());

        for endpoint in endpoints {
            let now = Utc::now();
            let mut record = WebhookDeliveryRecord {
                id: Uuid::new_v4().to_string(),
                endpoint_id: endpoint.id.clone(),
                event_type: event_type.to_string(),
                payload: payload_str.clone(),
                status: WebhookDeliveryStatus::Pending,
                attempt: 0,
                next_attempt_at: None,
                last_response: None,
                created_at: now,
                updated_at: now,
            };
            self.store.upsert(record.clone()).await?;
            self.attempt_delivery(&endpoint, &mut record).await?;
            results.push(record);
        }

        Ok(results)
    }

    /// Processes due pending deliveries up to `limit`, returning the count attempted.
    pub async fn process_due_deliveries(&self, limit: usize) -> Result<usize, WebhookError> {
        let due = self.store.list_due(Utc::now(), limit).await?;
        let mut attempted = 0;

        for mut record in due {
            let Some(endpoint) = self.endpoint_by_id(&record.endpoint_id) else {
                record.status = WebhookDeliveryStatus::Failed;
                record.next_attempt_at = None;
                record.last_response = Some("endpoint not registered".to_string());
                record.updated_at = Utc::now();
                self.store.upsert(record).await?;
                continue;
            };
            self.attempt_delivery(&endpoint, &mut record).await?;
            attempted += 1;
        }

        Ok(attempted)
    }

    async fn attempt_delivery(
        &self,
        endpoint: &WebhookEndpoint,
        record: &mut WebhookDeliveryRecord,
    ) -> Result<(), WebhookError> {
        let now = Utc::now();
        let ts = u64::try_from(now.timestamp().max(0)).unwrap_or(0);
        let body = record.payload.as_bytes().to_vec();
        let sig = sign_webhook_payload(&endpoint.secret, ts, &body);

        let send_res = self
            .transport
            .send(&endpoint.url, &sig, &record.event_type, &record.id, body)
            .await;

        record.attempt = record.attempt.saturating_add(1);
        record.updated_at = Utc::now();

        match send_res {
            Ok(resp) if (200..300).contains(&resp.status) => {
                record.status = WebhookDeliveryStatus::Delivered;
                record.next_attempt_at = None;
                record.last_response = Some(format!("HTTP {}", resp.status));
            }
            Ok(resp) => {
                record.last_response = Some(format!("HTTP {}", resp.status));
                self.schedule_retry_or_fail(record);
            }
            Err(err) => {
                record.last_response = Some(err.to_string());
                self.schedule_retry_or_fail(record);
            }
        }

        self.store.upsert(record.clone()).await
    }

    fn schedule_retry_or_fail(&self, record: &mut WebhookDeliveryRecord) {
        if let Some(delay) = self.retry_policy.next_backoff(record.attempt) {
            let delta = chrono::Duration::from_std(delay).unwrap_or_default();
            record.status = WebhookDeliveryStatus::Pending;
            record.next_attempt_at = Some(Utc::now() + delta);
        } else {
            record.status = WebhookDeliveryStatus::Failed;
            record.next_attempt_at = None;
        }
    }

    /// Builds an [`OutboxRelay`](crate::outbox::OutboxRelay) dispatcher closure that forwards
    /// outbox messages to this [`WebhookDispatcher`].
    pub fn outbox_dispatcher(
        dispatcher: Arc<WebhookDispatcher>,
    ) -> impl Fn(&OutboxMessage) -> BoxFuture<'static, Result<(), OutboxError>> + Send + Sync + 'static
    {
        move |msg: &OutboxMessage| {
            let dispatcher = Arc::clone(&dispatcher);
            let event_type = msg.event_type.clone();
            let payload_bytes = msg.payload.clone();
            Box::pin(async move {
                let payload: serde_json::Value = serde_json::from_slice(&payload_bytes)
                    .map_err(|e| OutboxError::Dispatch(format!("invalid JSON payload: {e}")))?;
                dispatcher
                    .dispatch_event(&event_type, payload)
                    .await
                    .map(|_| ())
                    .map_err(|e| OutboxError::Dispatch(e.to_string()))
            })
        }
    }
}

/// Background [`Job`] payload for dispatching a webhook event via [`JobQueue`](crate::jobs::JobQueue).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookDeliveryJob {
    /// Event type name.
    pub event_type: String,
    /// JSON payload to deliver.
    pub payload: serde_json::Value,
}

impl Job for WebhookDeliveryJob {
    const NAME: &'static str = "rume.webhook.deliver";
}

/// [`JobHandler`] that executes [`WebhookDeliveryJob`] via a [`WebhookDispatcher`].
#[derive(Debug, Clone)]
pub struct WebhookDeliveryJobHandler {
    dispatcher: Arc<WebhookDispatcher>,
}

impl WebhookDeliveryJobHandler {
    /// Creates a new [`WebhookDeliveryJobHandler`].
    pub fn new(dispatcher: Arc<WebhookDispatcher>) -> Self {
        Self { dispatcher }
    }
}

impl JobHandler<WebhookDeliveryJob> for WebhookDeliveryJobHandler {
    fn handle<'a>(
        &'a self,
        _ctx: &'a JobContext,
        job: WebhookDeliveryJob,
    ) -> crate::jobs::BoxFuture<'a, Result<(), JobError>> {
        Box::pin(async move {
            self.dispatcher
                .dispatch_event(&job.event_type, job.payload)
                .await
                .map(|_| ())
                .map_err(|e| JobError::Retryable(e.to_string()))
        })
    }
}
