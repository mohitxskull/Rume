//! Milestone D end-to-end integration tests through the `rume` facade.
//!
//! Exercises:
//! 1. `SqliteJobQueue` priority ordering + `SqlBatchStore`/`JobBatch` (`then`/`finally`) + `CronScheduler` leader election
//! 2. `RumeClient` against an `axum` server verifying W3C `traceparent`, `X-Request-ID`, retry, and `CircuitBreaker`
//! 3. `DistributedEventBus` two-node propagation, `SqlSagaStore` persistence, and `OutboxRelay` -> `InMemoryBroker`

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use rume::broker::{BrokerConsumer, BrokerMessage};
use rume::client::{OutboundHttpError, RetryConfig};
use rume::coordination::lock::CoordinationFuture;
use rume::coordination::{LeaderElector, LeaderLease};
use rume::jobs::{
    BatchBuilder, BatchStatus, BatchStore, BoxFuture, Job, JobContext, JobError, JobHandler,
    JobRegistry,
};
use rume::outbox::{OutboxMessage, OutboxRelay, OutboxRepository};
use rume::persistence::SqliteBackend;
use rume::prelude::*;
use rume::resilience::{CircuitBreaker, CircuitBreakerConfig, CircuitState};
use rume::saga::{SagaError, SagaStatus};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tokio::sync::{Notify, mpsc};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PriorityJob {
    label: String,
    prio: i32,
}

impl Job for PriorityJob {
    const NAME: &'static str = "milestone_d_priority_job";

    fn priority(&self) -> i32 {
        self.prio
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BatchWorkJob {
    item: u32,
}

impl Job for BatchWorkJob {
    const NAME: &'static str = "milestone_d_batch_work";
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BatchThenJob {
    tag: String,
}

impl Job for BatchThenJob {
    const NAME: &'static str = "milestone_d_batch_then";
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BatchFinallyJob {
    tag: String,
}

impl Job for BatchFinallyJob {
    const NAME: &'static str = "milestone_d_batch_finally";
}

struct RecordingHandler {
    events: Arc<Mutex<Vec<String>>>,
    label: &'static str,
}

impl<J: Job> JobHandler<J> for RecordingHandler {
    fn handle<'a>(&'a self, _ctx: &'a JobContext, _job: J) -> BoxFuture<'a, Result<(), JobError>> {
        let label = self.label;
        let events = Arc::clone(&self.events);
        Box::pin(async move {
            events.lock().expect("lock").push(label.to_string());
            Ok(())
        })
    }
}

#[derive(Debug, Default)]
struct InMemoryLeaderElector {
    leases: Mutex<HashMap<String, LeaderLease>>,
}

impl LeaderElector for InMemoryLeaderElector {
    fn try_acquire<'a>(
        &'a self,
        role: &'a str,
        candidate_id: &'a str,
        ttl: Duration,
    ) -> CoordinationFuture<'a, Option<LeaderLease>> {
        Box::pin(async move {
            let mut leases = self.leases.lock().expect("lock");
            let now = Instant::now();
            let next_term = match leases.get(role) {
                Some(existing) if !existing.is_expired() => {
                    if existing.candidate_id == candidate_id {
                        let updated = LeaderLease::new(role, candidate_id, existing.term)
                            .with_expires_at(now + ttl);
                        leases.insert(role.to_string(), updated.clone());
                        return Ok(Some(updated));
                    }
                    return Ok(None);
                }
                Some(existing) => existing.term + 1,
                None => 1,
            };
            let lease = LeaderLease::new(role, candidate_id, next_term).with_expires_at(now + ttl);
            leases.insert(role.to_string(), lease.clone());
            Ok(Some(lease))
        })
    }

    fn renew<'a>(
        &'a self,
        role: &'a str,
        candidate_id: &'a str,
        ttl: Duration,
    ) -> CoordinationFuture<'a, Option<LeaderLease>> {
        Box::pin(async move {
            let mut leases = self.leases.lock().expect("lock");
            let now = Instant::now();
            if let Some(existing) = leases.get_mut(role)
                && !existing.is_expired()
                && existing.candidate_id == candidate_id
            {
                existing.expires_at = Some(now + ttl);
                return Ok(Some(existing.clone()));
            }
            Ok(None)
        })
    }

    fn get_leader<'a>(&'a self, role: &'a str) -> CoordinationFuture<'a, Option<LeaderLease>> {
        Box::pin(async move {
            let leases = self.leases.lock().expect("lock");
            Ok(leases.get(role).filter(|l| !l.is_expired()).cloned())
        })
    }

    fn resign<'a>(&'a self, role: &'a str, candidate_id: &'a str) -> CoordinationFuture<'a, bool> {
        Box::pin(async move {
            let mut leases = self.leases.lock().expect("lock");
            if let Some(existing) = leases.get(role)
                && existing.candidate_id == candidate_id
            {
                leases.remove(role);
                return Ok(true);
            }
            Ok(false)
        })
    }
}

#[tokio::test]
async fn milestone_d_job_queue_priority_batch_and_cron_e2e() {
    let pool = SqlitePool::connect("sqlite::memory:")
        .await
        .expect("sqlite in-memory");
    let queue = SqliteJobQueue::new(pool.clone());
    queue.init_schema().await.expect("init jobs schema");

    // 1. Priority ordering in SqliteJobQueue
    queue
        .enqueue(PriorityJob {
            label: "low".into(),
            prio: -5,
        })
        .await
        .expect("enqueue low");
    queue
        .enqueue(PriorityJob {
            label: "high".into(),
            prio: 10,
        })
        .await
        .expect("enqueue high");

    let first = queue
        .dequeue_for("worker-1")
        .await
        .expect("dequeue first")
        .expect("first job present");
    let first_payload: PriorityJob = serde_json::from_slice(&first.payload).expect("decode first");
    assert_eq!(first_payload.label, "high");
    queue
        .ack(&first.id, first.lease_token.as_deref().unwrap_or_default())
        .await
        .expect("ack first");

    let second = queue
        .dequeue_for("worker-1")
        .await
        .expect("dequeue second")
        .expect("second job present");
    let second_payload: PriorityJob =
        serde_json::from_slice(&second.payload).expect("decode second");
    assert_eq!(second_payload.label, "low");
    queue
        .ack(
            &second.id,
            second.lease_token.as_deref().unwrap_or_default(),
        )
        .await
        .expect("ack second");

    // 2. JobBatch + SqlBatchStore with `then` & `finally` callbacks
    let batch_store = SqlBatchStore::Sqlite(pool);
    batch_store.init_schema().await.expect("init batch schema");

    let batch = BatchBuilder::new("e2e-batch")
        .add_job(BatchWorkJob { item: 1 })
        .add_job(BatchWorkJob { item: 2 })
        .then(BatchThenJob { tag: "done".into() })
        .finally(BatchFinallyJob {
            tag: "cleanup".into(),
        })
        .dispatch(&queue, &batch_store)
        .await
        .expect("dispatch batch");
    assert_eq!(batch.total_jobs, 2);

    for _ in 0..2 {
        let job = queue
            .dequeue_for("worker-1")
            .await
            .expect("dequeue batch item")
            .expect("batch item present");
        queue
            .ack(&job.id, job.lease_token.as_deref().unwrap_or_default())
            .await
            .expect("ack batch item");
        batch_store
            .record_job_success(&batch.id, &job.id)
            .await
            .expect("record completion");
    }

    let completed = batch_store
        .load_batch(&batch.id)
        .await
        .expect("get batch")
        .expect("batch exists");
    assert_eq!(completed.status, BatchStatus::Completed);

    let callback_events = Arc::new(Mutex::new(Vec::new()));
    let mut registry = JobRegistry::new();
    registry.register::<BatchThenJob, _>(RecordingHandler {
        events: Arc::clone(&callback_events),
        label: "then",
    });
    registry.register::<BatchFinallyJob, _>(RecordingHandler {
        events: Arc::clone(&callback_events),
        label: "finally",
    });

    completed
        .dispatch_callbacks(&batch_store, &registry)
        .await
        .expect("dispatch callbacks");
    let recorded = callback_events.lock().expect("lock").clone();
    assert_eq!(recorded, vec!["then".to_string(), "finally".to_string()]);

    // 3. CronScheduler leader election (only leader executes)
    let elector = Arc::new(InMemoryLeaderElector::default());
    let leader_runs = Arc::new(AtomicUsize::new(0));
    let follower_runs = Arc::new(AtomicUsize::new(0));
    let notify = Arc::new(Notify::new());

    let mut leader_sched = CronScheduler::new(
        Arc::clone(&elector) as Arc<dyn LeaderElector>,
        "e2e-cron",
        "node-1",
        Duration::from_secs(5),
    );
    let lr = Arc::clone(&leader_runs);
    let n = Arc::clone(&notify);
    leader_sched
        .register(
            CronJobSpec::new("every-sec", "* * * * * *").expect("valid cron"),
            move || {
                let lr = Arc::clone(&lr);
                let n = Arc::clone(&n);
                async move {
                    lr.fetch_add(1, Ordering::SeqCst);
                    n.notify_waiters();
                    Ok(())
                }
            },
        )
        .expect("register leader cron");

    let mut follower_sched = CronScheduler::new(
        Arc::clone(&elector) as Arc<dyn LeaderElector>,
        "e2e-cron",
        "node-2",
        Duration::from_secs(5),
    );
    let fr = Arc::clone(&follower_runs);
    follower_sched
        .register(
            CronJobSpec::new("every-sec", "* * * * * *").expect("valid cron"),
            move || {
                let fr = Arc::clone(&fr);
                async move {
                    fr.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            },
        )
        .expect("register follower cron");

    let h1 = leader_sched.start();
    // Wait deterministically until node-1 has acquired the leader lease before starting node-2.
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Ok(Some(lease)) = elector.get_leader("e2e-cron").await
                && lease.candidate_id == "node-1"
            {
                break;
            }
            tokio::task::yield_now().await;
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("node-1 must win leadership before node-2 starts");
    let h2 = follower_sched.start();

    tokio::time::timeout(Duration::from_secs(3), notify.notified())
        .await
        .expect("leader cron must fire within 3s");

    h1.shutdown(Duration::from_secs(2))
        .await
        .expect("shutdown h1");
    h2.shutdown(Duration::from_secs(2))
        .await
        .expect("shutdown h2");

    assert!(leader_runs.load(Ordering::SeqCst) >= 1);
    assert_eq!(follower_runs.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn milestone_d_rume_client_tracing_resilience_e2e() {
    type CapturedHeaders = Arc<Mutex<Vec<(Option<String>, Option<String>)>>>;

    let attempts = Arc::new(AtomicUsize::new(0));
    let captured_headers: CapturedHeaders = Arc::new(Mutex::new(Vec::new()));

    let app_attempts = Arc::clone(&attempts);
    let app_headers = Arc::clone(&captured_headers);
    let router = axum::Router::new()
        .route(
            "/flaky",
            get(move |headers: HeaderMap| {
                let attempts = Arc::clone(&app_attempts);
                let captured = Arc::clone(&app_headers);
                async move {
                    let tp = headers
                        .get("traceparent")
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_string);
                    let req_id = headers
                        .get("x-request-id")
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_string);
                    captured.lock().expect("lock").push((tp, req_id));

                    let n = attempts.fetch_add(1, Ordering::SeqCst) + 1;
                    if n < 3 {
                        (StatusCode::INTERNAL_SERVER_ERROR, "transient error")
                    } else {
                        (StatusCode::OK, "ok")
                    }
                }
            }),
        )
        .route(
            "/always-fail",
            get(|| async { (StatusCode::INTERNAL_SERVER_ERROR, "down") }),
        );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("local addr");
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });

    let breaker = Arc::new(CircuitBreaker::new(CircuitBreakerConfig {
        failure_threshold: 3,
        success_threshold: 1,
        reset_timeout: Duration::from_secs(30),
        ..Default::default()
    }));

    let client = RumeClient::builder()
        .circuit_breaker(Arc::clone(&breaker))
        .retry(RetryConfig {
            max_attempts: 3,
            initial_backoff: Duration::from_millis(5),
            max_backoff: Duration::from_millis(10),
            retry_non_idempotent: false,
        })
        .build()
        .expect("build RumeClient");

    let parent_tc = rume::app::TraceContext::generate();
    let expected_trace_id = parent_tc.trace_id().to_string();
    let flaky_url = format!("http://{addr}/flaky");
    let req = client.get(&flaky_url).build().expect("build request");
    let resp = parent_tc
        .scope_future(tracing::info_span!("outbound"), client.execute(req))
        .await
        .expect("retry should succeed");
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(attempts.load(Ordering::SeqCst), 3);

    let headers = captured_headers.lock().expect("lock").clone();
    assert_eq!(headers.len(), 3);
    for (tp, req_id) in headers.iter() {
        assert!(
            tp.as_deref()
                .is_some_and(|v| v.starts_with("00-") && v.contains(&expected_trace_id)),
            "expected W3C traceparent header with trace_id {expected_trace_id}, got {tp:?}"
        );
        assert!(
            req_id.as_deref().is_some_and(|v| !v.is_empty()),
            "expected X-Request-ID header, got {req_id:?}"
        );
    }

    // Trip the circuit breaker using /always-fail (max_attempts=3 -> 3 failures > threshold 2)
    let fail_url = format!("http://{addr}/always-fail");
    let fail_req = client.get(&fail_url).build().expect("build fail req");
    let _ = client.execute(fail_req).await;
    assert_eq!(breaker.state(), CircuitState::Open);

    let blocked_req = client.get(&fail_url).build().expect("build blocked req");
    let err = client
        .execute(blocked_req)
        .await
        .expect_err("open circuit must reject request");
    assert!(matches!(err, OutboundHttpError::CircuitOpen));

    server.abort();
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct OrderCreatedEvent {
    order_id: String,
}

impl Event for OrderCreatedEvent {
    const TYPE: &'static str = "milestone_d.order.created";
}

#[tokio::test]
async fn milestone_d_distributed_events_saga_and_broker_e2e() {
    // 1. DistributedEventBus two-node propagation
    let pubsub: Arc<dyn PubSub> = Arc::new(InMemoryPubSub::new());
    let node_a = DistributedEventBus::new(EventBus::new(), Arc::clone(&pubsub));
    let node_b = DistributedEventBus::new(EventBus::new(), Arc::clone(&pubsub));

    let (tx_b, mut rx_b) = mpsc::unbounded_channel();
    node_b
        .local()
        .subscribe_fn::<OrderCreatedEvent, _, _>(move |ev| {
            let tx_b = tx_b.clone();
            let order_id = ev.order_id.clone();
            async move {
                let _ = tx_b.send(order_id);
                Ok(())
            }
        });
    let _bridge_b = node_b
        .bridge::<OrderCreatedEvent>()
        .await
        .expect("bridge node_b");

    node_a
        .publish(&OrderCreatedEvent {
            order_id: "ord-42".into(),
        })
        .await
        .expect("publish from node_a");

    let received = tokio::time::timeout(Duration::from_secs(2), rx_b.recv())
        .await
        .expect("node_b should receive event")
        .expect("channel open");
    assert_eq!(received, "ord-42");

    // 2. SqlSagaStore persistence & compensation
    let saga_pool = SqlitePool::connect("sqlite::memory:")
        .await
        .expect("sqlite in-memory");
    let saga_store = Arc::new(SqlSagaStore::sqlite(saga_pool));
    saga_store.init_schema().await.expect("init saga schema");

    #[derive(Clone, Default)]
    struct SagaCtx {
        log: Arc<Mutex<Vec<&'static str>>>,
    }

    let saga = Saga::<SagaCtx>::new("checkout")
        .with_store(Arc::clone(&saga_store) as Arc<dyn SagaStore>)
        .step(
            "reserve",
            |ctx| {
                let log = Arc::clone(&ctx.log);
                async move {
                    log.lock().expect("lock").push("reserve");
                    Ok(())
                }
            },
            |ctx| {
                let log = Arc::clone(&ctx.log);
                async move {
                    log.lock().expect("lock").push("release");
                    Ok(())
                }
            },
        )
        .step(
            "charge",
            |_ctx| async {
                Err(SagaError::ActionFailed {
                    step: "charge".into(),
                    message: "declined".into(),
                })
            },
            |_ctx| async { Ok(()) },
        );

    let saga_id = "saga-e2e-1";
    let report = saga
        .run_with_id(saga_id, SagaCtx::default())
        .await
        .expect("compensated report");
    assert!(matches!(report.status, SagaStatus::Compensated { .. }));
    assert_eq!(
        *report.context.log.lock().expect("lock"),
        vec!["reserve", "release"]
    );

    let persisted = saga_store
        .load_record(saga_id)
        .await
        .expect("load saga record")
        .expect("record exists");
    assert!(matches!(persisted.status, SagaStatus::Compensated { .. }));

    // 3. OutboxRelay -> InMemoryBroker
    let backend = Arc::new(SqliteBackend::in_memory().await.expect("sqlite backend"));
    backend
        .init_outbox_table()
        .await
        .expect("init outbox table");

    let broker = Arc::new(InMemoryBroker::new());
    let mut consumer: Box<dyn BrokerConsumer> = broker
        .subscribe(OrderCreatedEvent::TYPE)
        .await
        .expect("subscribe broker");

    let msg = OutboxMessage::from_event(&OrderCreatedEvent {
        order_id: "ord-outbox-99".into(),
    })
    .expect("outbox message");
    backend.save(msg).await.expect("save outbox");

    let relay = OutboxRelay::with_broker(
        Arc::clone(&backend),
        Arc::clone(&broker) as Arc<dyn rume::broker::BrokerPublisher>,
    );
    let poll = relay
        .poll_and_dispatch(10)
        .await
        .expect("poll_and_dispatch");
    assert_eq!(poll.published, 1);

    let delivered: BrokerMessage =
        tokio::time::timeout(Duration::from_secs(2), consumer.next_message())
            .await
            .expect("broker receive timeout")
            .expect("broker recv ok")
            .expect("broker message present");
    assert_eq!(delivered.topic, OrderCreatedEvent::TYPE);
    let decoded: OrderCreatedEvent =
        serde_json::from_slice(&delivered.payload).expect("decode broker payload");
    assert_eq!(decoded.order_id, "ord-outbox-99");
}
