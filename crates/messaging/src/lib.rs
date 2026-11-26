//! Domain events, transactional outbox, and background job queues for Rume.

#![forbid(unsafe_code)]

pub mod broker;
pub mod cron;
pub mod events;
pub mod jobs;
pub mod mailer;
pub mod outbox;
pub mod pubsub;
pub mod webhooks;

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde::{Deserialize, Serialize};
    use tokio::sync::oneshot;

    use crate::events::{Event, EventBus};
    #[cfg(feature = "sqlite")]
    use crate::jobs::{Job, JobContext, JobError, JobHandler, JobRegistry, QueuedJob, RetryPolicy};
    #[cfg(feature = "sqlite")]
    use crate::outbox::{OutboxMessage, OutboxRelay, OutboxRepository};
    use rume_observability::TraceContext;

    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct TestEvent {
        id: String,
    }

    impl Event for TestEvent {
        const TYPE: &'static str = "test.event";
    }

    #[cfg(feature = "sqlite")]
    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct TestJob {
        id: String,
    }

    #[cfg(feature = "sqlite")]
    impl Job for TestJob {
        const NAME: &'static str = "test_job";
    }

    #[cfg(feature = "sqlite")]
    struct CapturingHandler {
        tx: tokio::sync::Mutex<Option<oneshot::Sender<Option<TraceContext>>>>,
    }

    #[cfg(feature = "sqlite")]
    impl JobHandler<TestJob> for CapturingHandler {
        fn handle<'a>(
            &'a self,
            _ctx: &'a JobContext,
            _job: TestJob,
        ) -> crate::jobs::BoxFuture<'a, Result<(), JobError>> {
            Box::pin(async move {
                if let Some(tx) = self.tx.lock().await.take() {
                    let _ = tx.send(TraceContext::current());
                }
                Ok(())
            })
        }
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn messaging_outbox_and_job_preserve_otel_trace_context_across_async_boundaries() {
        let parent_tc = TraceContext::generate();
        let parent_trace_id = parent_tc.trace_id().to_string();
        let parent_span_id = parent_tc.span_id().to_string();

        let backend = Arc::new(
            rume_data::persistence::SqliteBackend::in_memory()
                .await
                .expect("sqlite in-memory"),
        );
        backend
            .init_outbox_table()
            .await
            .expect("init outbox table");

        let (msg, queued) = parent_tc
            .scope_future(tracing::info_span!("http.request"), async {
                let msg = OutboxMessage::new("msg-1", "order.created", br#"{"ok":true}"#.to_vec());
                let job_ctx = JobContext::new("j-1", 1, 3);
                let queued = QueuedJob {
                    id: "j-1".into(),
                    name: TestJob::NAME.into(),
                    queue: "default".into(),
                    priority: 0,
                    payload: serde_json::to_vec(&TestJob { id: "j-1".into() }).expect("json"),
                    attempt: 1,
                    retry_policy: RetryPolicy::default(),
                    timeout: None,
                    idempotency_key: None,
                    tenant_id: None,
                    assigned_worker: None,
                    lease_token: None,
                    available_at: std::time::Instant::now(),
                    lease_duration: std::time::Duration::from_secs(30),
                    trace_context: job_ctx.trace_context,
                };
                (msg, queued)
            })
            .await;

        assert!(
            msg.trace_context
                .as_deref()
                .is_some_and(|s| s.contains(&parent_trace_id)),
            "outbox message must capture active OTel trace_id"
        );
        assert!(
            queued
                .trace_context
                .as_deref()
                .is_some_and(|s| s.contains(&parent_trace_id)),
            "queued job must capture active OTel trace_id"
        );

        backend.save(msg).await.expect("save outbox");
        let (outbox_tx, outbox_rx) = oneshot::channel();
        let outbox_tx = Arc::new(tokio::sync::Mutex::new(Some(outbox_tx)));
        let relay = OutboxRelay::new(Arc::clone(&backend), move |_msg| {
            let tx = Arc::clone(&outbox_tx);
            Box::pin(async move {
                if let Some(sender) = tx.lock().await.take() {
                    let _ = sender.send(TraceContext::current());
                }
                Ok(())
            })
        });
        let polled = relay
            .poll_and_dispatch(10)
            .await
            .expect("poll_and_dispatch");
        assert_eq!(polled.published, 1);

        let relay_tc = outbox_rx
            .await
            .expect("relay channel")
            .expect("relay TraceContext::current()");
        assert_eq!(relay_tc.trace_id(), parent_trace_id);
        assert_ne!(relay_tc.span_id(), parent_span_id);

        let (job_tx, job_rx) = oneshot::channel();
        let mut registry = JobRegistry::new();
        registry.register(CapturingHandler {
            tx: tokio::sync::Mutex::new(Some(job_tx)),
        });
        registry.dispatch(&queued).await.expect("dispatch job");

        let job_tc = job_rx
            .await
            .expect("job channel")
            .expect("job TraceContext::current()");
        assert_eq!(job_tc.trace_id(), parent_trace_id);
        assert_ne!(job_tc.span_id(), parent_span_id);
    }

    #[tokio::test]
    async fn messaging_event_bus_propagates_otel_parent_span_to_subscribers() {
        let parent_tc = TraceContext::generate();
        let parent_trace_id = parent_tc.trace_id().to_string();

        let bus = EventBus::new();
        let (tx, rx) = oneshot::channel();
        let tx = Arc::new(tokio::sync::Mutex::new(Some(tx)));
        bus.subscribe_fn::<TestEvent, _, _>(move |_ev| {
            let tx = Arc::clone(&tx);
            Box::pin(async move {
                if let Some(sender) = tx.lock().await.take() {
                    let _ = sender.send(TraceContext::current());
                }
                Ok(())
            })
        });

        let count = parent_tc
            .scope_future(tracing::info_span!("publisher"), async {
                bus.publish_async(TestEvent { id: "ev-1".into() })
            })
            .await;
        assert_eq!(count, 1);

        let sub_tc = rx
            .await
            .expect("subscriber channel")
            .expect("subscriber TraceContext::current()");
        assert_eq!(sub_tc.trace_id(), parent_trace_id);
    }
}
