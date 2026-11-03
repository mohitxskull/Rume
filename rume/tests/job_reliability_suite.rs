#![cfg(all(feature = "postgres", feature = "redis"))]

use std::sync::Arc;
use std::time::Duration;

use rume::jobs::{
    HeartbeatRegistry, IdempotencyStatus, IdempotencyStore, JobQueue, PostgresJobQueue,
    RedisHeartbeatRegistry, RedisIdempotencyStore,
};
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::redis::Redis;
use tokio::time::sleep;

mod common;

async fn start_redis_pool() -> (
    &'static testcontainers_modules::testcontainers::ContainerAsync<Redis>,
    deadpool_redis::Pool,
) {
    let node = common::containers::shared_redis().await;
    let pool = common::containers::create_isolated_redis_pool().await;
    (node, pool)
}

async fn start_postgres_queue() -> (
    &'static testcontainers_modules::testcontainers::ContainerAsync<Postgres>,
    Arc<PostgresJobQueue>,
) {
    let node = common::containers::shared_postgres().await;
    let (_db_name, config) = common::containers::create_isolated_postgres_db().await;
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&config.url)
        .await
        .expect("pg pool");
    let queue = Arc::new(PostgresJobQueue::new(pool));
    queue.init_schema().await.expect("schema");
    (node, queue)
}

#[tokio::test]
async fn idempotency_store_lifecycle() {
    let (_node, pool) = start_redis_pool().await;
    let store = RedisIdempotencyStore::new(pool);

    // 1. Initial attempt: New
    let status = store
        .try_acquire("req_order_123", Duration::from_secs(10))
        .await
        .unwrap();
    assert_eq!(status, IdempotencyStatus::New);

    // 2. Second attempt while in progress: InProgress
    let status2 = store
        .try_acquire("req_order_123", Duration::from_secs(10))
        .await
        .unwrap();
    assert_eq!(status2, IdempotencyStatus::InProgress);

    // 3. Complete execution
    store
        .complete("req_order_123", Some(b"order_created".to_vec()))
        .await
        .unwrap();

    // 4. Third attempt after completion: Completed with saved result
    let status3 = store
        .try_acquire("req_order_123", Duration::from_secs(10))
        .await
        .unwrap();
    assert_eq!(
        status3,
        IdempotencyStatus::Completed(Some(b"order_created".to_vec()))
    );

    // 5. Another key can be released
    let status_other = store
        .try_acquire("req_fail_456", Duration::from_secs(10))
        .await
        .unwrap();
    assert_eq!(status_other, IdempotencyStatus::New);

    store.release("req_fail_456").await.unwrap();

    let status_retry = store
        .try_acquire("req_fail_456", Duration::from_secs(10))
        .await
        .unwrap();
    assert_eq!(status_retry, IdempotencyStatus::New);
}

#[tokio::test]
async fn worker_heartbeat_tracking_and_reaping() {
    let (_node, pool) = start_redis_pool().await;
    let tracker = RedisHeartbeatRegistry::new(pool);

    // Worker 1 alive with 100ms TTL
    tracker
        .record_heartbeat("worker_1", Duration::from_millis(100))
        .await
        .unwrap();

    // Worker 2 alive with 2000ms TTL
    tracker
        .record_heartbeat("worker_2", Duration::from_secs(2))
        .await
        .unwrap();

    assert!(tracker.is_alive("worker_1").await.unwrap());
    assert!(tracker.is_alive("worker_2").await.unwrap());

    // Sleep past Worker 1 TTL
    sleep(Duration::from_millis(150)).await;

    assert!(!tracker.is_alive("worker_1").await.unwrap());
    assert!(tracker.is_alive("worker_2").await.unwrap());

    // Reap dead workers
    let dead = tracker.reap_dead_workers().await.unwrap();
    assert_eq!(dead, vec!["worker_1"]);

    // Worker 2 is still active
    assert!(tracker.is_alive("worker_2").await.unwrap());
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct IdempotentPaymentJob {
    payment_id: String,
    amount: u64,
}

impl rume::jobs::Job for IdempotentPaymentJob {
    const NAME: &'static str = "idempotent_payment";

    fn idempotency_key(&self) -> Option<String> {
        Some(format!("pay:{}", self.payment_id))
    }
}

struct PaymentHandler {
    executed: std::sync::Arc<std::sync::atomic::AtomicU32>,
}

impl rume::jobs::JobHandler<IdempotentPaymentJob> for PaymentHandler {
    fn handle<'a>(
        &'a self,
        _ctx: &'a rume::jobs::JobContext,
        _job: IdempotentPaymentJob,
    ) -> rume::jobs::BoxFuture<'a, Result<(), rume::jobs::JobError>> {
        let executed = std::sync::Arc::clone(&self.executed);
        Box::pin(async move {
            executed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        })
    }
}

#[tokio::test]
async fn test_worker_idempotency_deduplication() {
    let (_pg_node, queue) = start_postgres_queue().await;
    let (_redis_node, redis_pool) = start_redis_pool().await;
    let idempotency_store = Arc::new(RedisIdempotencyStore::new(redis_pool));

    let mut registry = rume::jobs::JobRegistry::new();
    let executed_counter = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));

    registry.register(PaymentHandler {
        executed: std::sync::Arc::clone(&executed_counter),
    });

    let worker =
        rume::jobs::JobWorker::new(std::sync::Arc::clone(&queue), std::sync::Arc::new(registry))
            .with_idempotency_store(idempotency_store);
    worker.start();

    // Enqueue job with same idempotency key twice
    queue
        .enqueue(IdempotentPaymentJob {
            payment_id: "tx_1001".into(),
            amount: 500,
        })
        .await
        .unwrap();

    sleep(Duration::from_millis(50)).await;
    assert_eq!(
        executed_counter.load(std::sync::atomic::Ordering::SeqCst),
        1
    );

    // Enqueue second job with identical idempotency key
    queue
        .enqueue(IdempotentPaymentJob {
            payment_id: "tx_1001".into(),
            amount: 500,
        })
        .await
        .unwrap();

    sleep(Duration::from_millis(50)).await;
    // Execution count must remain 1!
    assert_eq!(
        executed_counter.load(std::sync::atomic::Ordering::SeqCst),
        1
    );

    let _ = worker.drain(Duration::from_millis(100)).await;
}

#[tokio::test]
async fn test_worker_heartbeat_and_abandoned_job_recovery() {
    let (_pg_node, queue) = start_postgres_queue().await;
    let (_redis_node, redis_pool) = start_redis_pool().await;
    let heartbeats: Arc<dyn HeartbeatRegistry> = Arc::new(RedisHeartbeatRegistry::new(redis_pool));
    let registry = Arc::new(rume::jobs::JobRegistry::new());

    // Worker 1 starts with heartbeat and abandoned recovery
    let worker1 = rume::jobs::JobWorker::new(Arc::clone(&queue), registry)
        .with_worker_id("worker-active-1")
        .with_heartbeat(
            Arc::clone(&heartbeats),
            Duration::from_millis(400),
            Duration::from_millis(100),
        )
        .with_abandoned_recovery(Duration::from_millis(200));
    worker1.start();

    // Verify worker 1 registered heartbeat
    sleep(Duration::from_millis(150)).await;
    assert!(heartbeats.is_alive("worker-active-1").await.unwrap());

    // Simulate an abandoned job belonging to a crashed worker
    heartbeats
        .record_heartbeat("crashed-worker-99", Duration::from_millis(100))
        .await
        .unwrap();

    // Enqueue and claim a job under crashed-worker-99
    queue
        .enqueue(IdempotentPaymentJob {
            payment_id: "tx_abandoned".into(),
            amount: 100,
        })
        .await
        .unwrap();
    let claimed = queue.dequeue_for("crashed-worker-99").await.unwrap();
    assert!(claimed.is_some());
    assert_eq!(queue.pending_count().await.unwrap(), 0);

    // Sleep past crashed-worker-99 TTL so worker1 recovery loop detects it and reclaims
    sleep(Duration::from_millis(400)).await;

    // The abandoned job should now be reclaimed back into pending
    assert_eq!(queue.pending_count().await.unwrap(), 1);

    let _ = worker1.drain(Duration::from_millis(100)).await;
}
