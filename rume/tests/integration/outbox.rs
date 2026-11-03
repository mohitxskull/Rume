#![cfg(feature = "sqlite")]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use rume::events::Event;
use rume::outbox::{
    OutboxError, OutboxMessage, OutboxRelay, OutboxRelayConfig, OutboxRepository, OutboxStatus,
};
use rume::persistence::SqliteBackend;
use serde::{Deserialize, Serialize};

async fn setup_real_outbox() -> Arc<SqliteBackend> {
    let backend = SqliteBackend::in_memory().await.expect("sqlite in memory");
    backend.init_outbox_table().await.expect("init outbox table");
    Arc::new(backend)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct OrderPaidEvent {
    order_id: String,
    amount_cents: u64,
}

impl Event for OrderPaidEvent {
    const TYPE: &'static str = "order.paid";
}

#[tokio::test]
async fn outbox_message_creation_and_payload_serialization() {
    let event = OrderPaidEvent {
        order_id: "order_99".into(),
        amount_cents: 4999,
    };

    let msg = OutboxMessage::from_event(&event).expect("outbox message creation");
    assert_eq!(msg.event_type, "order.paid");
    assert_eq!(msg.status, OutboxStatus::Pending);
    assert_eq!(msg.retry_count, 0);

    let deserialized: OrderPaidEvent = msg.deserialize_payload().expect("deserialize payload");
    assert_eq!(deserialized, event);
}

#[tokio::test]
async fn outbox_relay_drains_and_dispatches_pending_messages() {
    let repo = setup_real_outbox().await;
    let dispatched_counter = Arc::new(AtomicUsize::new(0));

    // Save 3 messages to the repository
    for i in 1..=3 {
        let event = OrderPaidEvent {
            order_id: format!("ord_{i}"),
            amount_cents: 1000 * i,
        };
        let msg = OutboxMessage::from_event(&event).unwrap();
        repo.save(msg).await.unwrap();
    }

    assert_eq!(repo.pending_count().await.unwrap(), 3);
    assert_eq!(repo.published_count().await.unwrap(), 0);

    let counter_clone = Arc::clone(&dispatched_counter);
    let relay = OutboxRelay::new(Arc::clone(&repo), move |_msg| {
        let cnt = Arc::clone(&counter_clone);
        async move {
            cnt.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    });

    // Poll and dispatch batch
    let processed = relay.poll_and_dispatch(10).await.unwrap();
    assert_eq!(processed, 3);
    assert_eq!(dispatched_counter.load(Ordering::SeqCst), 3);

    assert_eq!(repo.pending_count().await.unwrap(), 0);
    assert_eq!(repo.published_count().await.unwrap(), 3);
}

#[tokio::test]
async fn outbox_relay_tracks_failures_and_exhaustion() {
    let repo = setup_real_outbox().await;

    let event = OrderPaidEvent {
        order_id: "failing_ord".into(),
        amount_cents: 500,
    };
    let msg = OutboxMessage::from_event(&event).unwrap();
    let msg_id = msg.id.clone();
    repo.save(msg).await.unwrap();

    let relay = OutboxRelay::new(Arc::clone(&repo), |_msg| async move {
        Err(OutboxError::Dispatch("kafka cluster unreachable".into()))
    });

    // 1st failure via relay
    let _ = relay.poll_and_dispatch(1).await;

    // Immediately calling poll_and_dispatch again does NOT refetch because next_retry_at is in the future!
    let claimed_immediate = relay.poll_and_dispatch(1).await.unwrap();
    assert_eq!(
        claimed_immediate, 0,
        "Backoff should prevent immediate refetch during outage"
    );

    // Verify backoff and failure tracking directly with lease token fencing
    use rume::persistence::DatabaseBackend;
    for _ in 0..3 {
        repo.execute(&format!("UPDATE rume_outbox SET next_retry_at = NULL WHERE id = '{msg_id}'")).await.unwrap();
        let claimed = repo.claim_pending(1).await.unwrap();
        assert_eq!(claimed.len(), 1);
        let token = claimed[0].lease_token.as_ref().unwrap();
        repo.mark_failed(&msg_id, token, "cluster down".into())
            .await
            .unwrap();
    }

    // 5th failure -> status transitions to Failed (DLQ)
    repo.execute(&format!("UPDATE rume_outbox SET next_retry_at = NULL WHERE id = '{msg_id}'")).await.unwrap();
    let claimed = repo.claim_pending(1).await.unwrap();
    assert_eq!(claimed.len(), 1);
    let token = claimed[0].lease_token.as_ref().unwrap();
    repo.mark_failed(&msg_id, token, "cluster down".into())
        .await
        .unwrap();

    let pending_after = repo.fetch_pending(1).await.unwrap();
    assert_eq!(pending_after.len(), 0); // No longer returned as Pending

    assert_eq!(repo.pending_count().await.unwrap(), 0);
    assert_eq!(repo.failed_count().await.unwrap(), 1);

    // Verify DLQ fetch and reprocessing
    let failed = repo.fetch_failed(10).await.unwrap();
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].id, msg_id);

    repo.reprocess_failed(&msg_id).await.unwrap();
    assert_eq!(repo.pending_count().await.unwrap(), 1);
    assert_eq!(repo.failed_count().await.unwrap(), 0);
}

#[tokio::test]
async fn outbox_relay_graceful_drain_on_shutdown() {
    let repo = setup_real_outbox().await;
    let dispatched = Arc::new(AtomicUsize::new(0));

    // Queue 2 messages
    for i in 1..=2 {
        let event = OrderPaidEvent {
            order_id: format!("drain_{i}"),
            amount_cents: 2000,
        };
        let msg = OutboxMessage::from_event(&event).unwrap();
        repo.save(msg).await.unwrap();
    }

    let cnt = Arc::clone(&dispatched);
    let relay = OutboxRelay::new(Arc::clone(&repo), move |_msg| {
        let cnt = Arc::clone(&cnt);
        async move {
            cnt.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    });

    // Drain should process remaining messages and return true
    let drained = relay.drain(Duration::from_millis(300)).await;
    assert!(drained);
    assert_eq!(dispatched.load(Ordering::SeqCst), 2);
    assert_eq!(repo.pending_count().await.unwrap(), 0);
}

#[tokio::test]
async fn concurrent_relays_claim_disjoint_batches() {
    let repo = setup_real_outbox().await;

    for i in 1..=10 {
        let event = OrderPaidEvent {
            order_id: format!("claim_{i}"),
            amount_cents: 100 * i,
        };
        let msg = OutboxMessage::from_event(&event).unwrap();
        repo.save(msg).await.unwrap();
    }

    // Two concurrent claims of 5 messages each
    let repo1 = Arc::clone(&repo);
    let repo2 = Arc::clone(&repo);

    let h1 = tokio::spawn(async move { repo1.claim_pending(5).await.unwrap() });
    let h2 = tokio::spawn(async move { repo2.claim_pending(5).await.unwrap() });

    let (res1, res2) = tokio::join!(h1, h2);
    let batch1 = res1.unwrap();
    let batch2 = res2.unwrap();

    assert_eq!(batch1.len(), 5);
    assert_eq!(batch2.len(), 5);

    // Confirm disjoint set of IDs
    let set1: std::collections::HashSet<_> = batch1.iter().map(|m| &m.id).collect();
    for msg in &batch2 {
        assert!(
            !set1.contains(&msg.id),
            "batch2 must not contain message already claimed by batch1"
        );
    }

    // Nothing pending left to claim
    let remaining = repo.claim_pending(10).await.unwrap();
    assert_eq!(remaining.len(), 0);
}

#[tokio::test]
async fn outbox_relay_drain_fails_closed_when_pending_count_errors() {
    use rume::outbox::BoxFuture;

    #[derive(Debug)]
    struct FailingRepo;
    impl OutboxRepository for FailingRepo {
        fn save(&self, _msg: OutboxMessage) -> BoxFuture<'_, Result<(), OutboxError>> {
            Box::pin(async { Ok(()) })
        }
        fn fetch_pending(
            &self,
            _limit: usize,
        ) -> BoxFuture<'_, Result<Vec<OutboxMessage>, OutboxError>> {
            Box::pin(async { Ok(vec![]) })
        }
        fn claim_pending(
            &self,
            _limit: usize,
        ) -> BoxFuture<'_, Result<Vec<OutboxMessage>, OutboxError>> {
            Box::pin(async { Ok(vec![]) })
        }
        fn claim_pending_with_lease(
            &self,
            _limit: usize,
            _lease_duration_secs: u64,
        ) -> BoxFuture<'_, Result<Vec<OutboxMessage>, OutboxError>> {
            Box::pin(async { Ok(vec![]) })
        }
        fn mark_published(
            &self,
            _id: &str,
            _lease_token: &str,
        ) -> BoxFuture<'_, Result<(), OutboxError>> {
            Box::pin(async { Ok(()) })
        }
        fn mark_failed(
            &self,
            _id: &str,
            _lease_token: &str,
            _reason: String,
        ) -> BoxFuture<'_, Result<(), OutboxError>> {
            Box::pin(async { Ok(()) })
        }
        fn pending_count(&self) -> BoxFuture<'_, Result<usize, OutboxError>> {
            Box::pin(async { Err(OutboxError::Repository("database offline".into())) })
        }
        fn published_count(&self) -> BoxFuture<'_, Result<usize, OutboxError>> {
            Box::pin(async { Ok(0) })
        }
    }

    let repo = Arc::new(FailingRepo);
    let relay = OutboxRelay::new(repo, |_msg| async { Ok(()) });

    let try_result = relay.try_drain(Duration::from_millis(50)).await;
    assert!(
        try_result.is_err(),
        "try_drain must propagate repository error"
    );

    let drain_result = relay.drain(Duration::from_millis(50)).await;
    assert!(
        !drain_result,
        "drain must return false when repository errors"
    );
}

#[tokio::test]
async fn transactional_outbox_commits_atomically_with_business_mutation() {
    use rume::outbox::TransactionalOutbox;
    use rume::persistence::{
        DatabaseBackend, DatabaseConfig, DatabasePool, DatabaseTransaction, PersistenceError,
        SqliteBackend,
    };

    let backend = Arc::new(SqliteBackend::in_memory().await.unwrap());
    backend.init_outbox_table().await.unwrap();
    backend
        .execute("CREATE TABLE orders (id TEXT PRIMARY KEY, status TEXT NOT NULL);")
        .await
        .unwrap();

    let pool = DatabasePool::new((*backend).clone(), DatabaseConfig::default());

    let event = OrderPaidEvent {
        order_id: "order_tx_success".into(),
        amount_cents: 9900,
    };
    let msg = OutboxMessage::from_event(&event).unwrap();
    let msg_id = msg.id.clone();

    // Execute atomic business update + outbox enlistment
    pool.transaction(|tx| {
        let b = Arc::clone(&backend);
        async move {
            tx.execute("INSERT INTO orders (id, status) VALUES ('order_tx_success', 'completed');")
                .await?;
            b.save_tx(&tx, msg).await.unwrap();
            Ok::<(), PersistenceError>(())
        }
    })
    .await
    .unwrap();

    // Verify both business data AND outbox message committed
    let order_status = pool
        .backend()
        .query_scalar("SELECT status FROM orders WHERE id = 'order_tx_success';")
        .await
        .unwrap();
    assert_eq!(order_status.as_deref(), Some("completed"));

    let pending = backend.fetch_pending(10).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, msg_id);
    assert_eq!(pending[0].event_type, "order.paid");
}

#[tokio::test]
async fn transactional_outbox_rolls_back_atomically_on_transaction_failure() {
    use rume::outbox::TransactionalOutbox;
    use rume::persistence::{
        DatabaseBackend, DatabaseConfig, DatabasePool, DatabaseTransaction, PersistenceError,
        SqliteBackend,
    };

    let backend = Arc::new(SqliteBackend::in_memory().await.unwrap());
    backend.init_outbox_table().await.unwrap();
    backend
        .execute("CREATE TABLE orders (id TEXT PRIMARY KEY, status TEXT NOT NULL);")
        .await
        .unwrap();

    let pool = DatabasePool::new((*backend).clone(), DatabaseConfig::default());

    let event = OrderPaidEvent {
        order_id: "order_tx_fail".into(),
        amount_cents: 4500,
    };
    let msg = OutboxMessage::from_event(&event).unwrap();

    // Transaction fails after enlisting outbox message
    let tx_res = pool
        .transaction(|tx| {
            let b = Arc::clone(&backend);
            async move {
                tx.execute("INSERT INTO orders (id, status) VALUES ('order_tx_fail', 'pending');")
                    .await?;
                b.save_tx(&tx, msg).await.unwrap();
                Err::<(), PersistenceError>(PersistenceError::TransactionError(
                    "simulated database abort".into(),
                ))
            }
        })
        .await;

    assert!(tx_res.is_err());

    // Both the business mutation AND the outbox message must be rolled back!
    let order_status = pool
        .backend()
        .query_scalar("SELECT status FROM orders WHERE id = 'order_tx_fail';")
        .await
        .unwrap();
    assert_eq!(order_status, None);

    let pending = backend.fetch_pending(10).await.unwrap();
    assert_eq!(
        pending.len(),
        0,
        "rolled back outbox message must not be visible"
    );
}

#[tokio::test]
async fn outbox_claim_lease_recovery_of_abandoned_messages() {
    let repo = setup_real_outbox().await;

    let event = OrderPaidEvent {
        order_id: "order_abandoned".into(),
        amount_cents: 1200,
    };
    let msg = OutboxMessage::from_event(&event).unwrap();
    repo.save(msg).await.unwrap();

    // 1. Worker claims the message with a 0-second lease (expires immediately)
    let claimed = repo.claim_pending_with_lease(1, 0).await.unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].status, OutboxStatus::InFlight);
    assert!(claimed[0].lease_token.is_some());
    assert!(claimed[0].lease_expires_at.is_some());

    // Another worker immediate claim gets 0 messages
    let claimed_second = repo.claim_pending(1).await.unwrap();
    assert_eq!(claimed_second.len(), 0);

    // 2. Simulating worker crash: time passes (lease expires after 0s with max_lease_age_secs = 0)
    let recovered_count = repo.recover_expired_claims(0).await.unwrap();
    assert_eq!(recovered_count, 1, "abandoned message must be recovered");

    // 3. Message is now Pending again and can be claimed and published by the relay
    let dispatched = Arc::new(AtomicUsize::new(0));
    let cnt = Arc::clone(&dispatched);
    let relay = OutboxRelay::new(Arc::clone(&repo), move |_msg| {
        let cnt = Arc::clone(&cnt);
        async move {
            cnt.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    });

    let published_count = relay.poll_and_dispatch(5).await.unwrap();
    assert_eq!(published_count, 1);
    assert_eq!(dispatched.load(Ordering::SeqCst), 1);
    assert_eq!(repo.pending_count().await.unwrap(), 0);
}

#[tokio::test]
async fn outbox_superseded_worker_cannot_mark_published_or_failed() {
    let repo = setup_real_outbox().await;

    let event = OrderPaidEvent {
        order_id: "order_fencing".into(),
        amount_cents: 2500,
    };
    let msg = OutboxMessage::from_event(&event).unwrap();
    let msg_id = msg.id.clone();
    repo.save(msg).await.unwrap();

    // Worker A claims with 0s lease
    let claimed_a = repo.claim_pending_with_lease(1, 0).await.unwrap();
    assert_eq!(claimed_a.len(), 1);
    let token_a = claimed_a[0].lease_token.clone().unwrap();

    // 1. Worker A attempts mark_published with invalid token -> rejected
    let bad_res = repo.mark_published(&msg_id, "invalid-token").await;
    assert!(bad_res.is_err(), "invalid token must be rejected");

    // 2. Recovery runs because lease duration was 0s
    let recovered = repo.recover_expired_claims(0).await.unwrap();
    assert_eq!(recovered, 1);

    // 3. Worker B claims the message
    let claimed_b = repo.claim_pending_with_lease(1, 60).await.unwrap();
    assert_eq!(claimed_b.len(), 1);
    let token_b = claimed_b[0].lease_token.clone().unwrap();
    assert_ne!(token_a, token_b);

    // 4. Superseded Worker A attempts mark_published with token_a -> rejected!
    let a_pub_res = repo.mark_published(&msg_id, &token_a).await;
    assert!(a_pub_res.is_err(), "superseded worker A must not mark published");

    // 5. Superseded Worker A attempts mark_failed with token_a -> rejected!
    let a_fail_res = repo.mark_failed(&msg_id, &token_a, "failed".into()).await;
    assert!(a_fail_res.is_err(), "superseded worker A must not mark failed");

    // 6. Active Worker B marks published with token_b -> succeeds
    let b_pub_res = repo.mark_published(&msg_id, &token_b).await;
    assert!(b_pub_res.is_ok(), "active worker B must succeed");
    assert_eq!(repo.published_count().await.unwrap(), 1);
}

#[tokio::test]
async fn outbox_recovery_does_not_prematurely_revoke_valid_lease_on_old_message() {
    let repo = setup_real_outbox().await;

    let event = OrderPaidEvent {
        order_id: "order_old_queue".into(),
        amount_cents: 3000,
    };
    let msg = OutboxMessage::from_event(&event).unwrap();
    let msg_id = msg.id.clone();
    repo.save(msg).await.unwrap();

    // Simulate message queued for 100 seconds
    use rume::persistence::DatabaseBackend;
    repo.execute(&format!("UPDATE rume_outbox SET created_at = created_at - 100 WHERE id = '{msg_id}'"))
        .await
        .unwrap();

    // Worker claims message with a 60-second lease
    let claimed = repo.claim_pending_with_lease(1, 60).await.unwrap();
    assert_eq!(claimed.len(), 1);
    let token = claimed[0].lease_token.clone().unwrap();

    // Another relay executes recovery (e.g. recover_expired_claims(30))
    // Message is 100s old, but its lease expires in 60s, so it MUST NOT be reclaimed!
    let recovered = repo.recover_expired_claims(30).await.unwrap();
    assert_eq!(recovered, 0, "active 60s lease on old message must NOT be revoked");

    // Original worker completes successfully
    repo.mark_published(&msg_id, &token).await.unwrap();
    assert_eq!(repo.published_count().await.unwrap(), 1);
}

#[tokio::test]
async fn outbox_duplicate_insert_preserves_published_and_inflight_state() {
    let repo = setup_real_outbox().await;

    let event = OrderPaidEvent {
        order_id: "order_idempotent_1".into(),
        amount_cents: 9999,
    };
    let msg = OutboxMessage::from_event(&event).unwrap();
    let msg_id = msg.id.clone();
    repo.save(msg.clone()).await.unwrap();

    // Claim and publish
    let claimed = repo.claim_pending_with_lease(1, 60).await.unwrap();
    assert_eq!(claimed.len(), 1);
    let token = claimed[0].lease_token.clone().unwrap();
    repo.mark_published(&msg_id, &token).await.unwrap();
    assert_eq!(repo.published_count().await.unwrap(), 1);
    assert_eq!(repo.pending_count().await.unwrap(), 0);

    // Client idempotently retries request: save() with same message ID
    repo.save(msg).await.unwrap();

    // Must remain published, not reset to pending!
    assert_eq!(repo.published_count().await.unwrap(), 1);
    assert_eq!(repo.pending_count().await.unwrap(), 0);
}

#[tokio::test]
async fn outbox_relay_recovers_from_async_dispatcher_panic() {
    let repo = setup_real_outbox().await;

    let event = OrderPaidEvent {
        order_id: "order_panic_1".into(),
        amount_cents: 100,
    };
    let msg = OutboxMessage::from_event(&event).unwrap();
    let msg_id = msg.id.clone();
    repo.save(msg).await.unwrap();

    let panic_hit = Arc::new(AtomicUsize::new(0));
    let hit_clone = Arc::clone(&panic_hit);

    let relay = OutboxRelay::new(Arc::clone(&repo), move |_msg: &OutboxMessage| {
        let hit = Arc::clone(&hit_clone);
        async move {
            hit.fetch_add(1, Ordering::SeqCst);
            panic!("unhandled async dispatcher panic");
            #[allow(unreachable_code)]
            Ok(())
        }
    });
    let published = relay.poll_and_dispatch(10).await.unwrap();

    assert_eq!(published, 0);
    assert_eq!(panic_hit.load(Ordering::SeqCst), 1);

    // Durable outbox invariant: message failure recorded with panic error and retry backoff scheduled
    use sqlx::Row;
    let row = sqlx::query("SELECT retry_count, last_error, lease_token FROM rume_outbox WHERE id = ?1")
        .bind(&msg_id)
        .fetch_one(repo.pool())
        .await
        .unwrap();
    let retry_count: i32 = row.get(0);
    let last_error: Option<String> = row.get(1);
    let lease_token: Option<String> = row.get(2);

    assert_eq!(retry_count, 1);
    assert!(last_error.unwrap_or_default().contains("panicked"));
    assert_eq!(lease_token, None, "lease token cleared on failure");
}

#[tokio::test]
async fn outbox_lease_expiry_rejects_completion_before_another_claims() {
    let repo = setup_real_outbox().await;

    let event = OrderPaidEvent {
        order_id: "order_expiry_1".into(),
        amount_cents: 2000,
    };
    let msg = OutboxMessage::from_event(&event).unwrap();
    let msg_id = msg.id.clone();
    repo.save(msg).await.unwrap();

    // Claim with 0s lease duration -> immediately expired against database clock
    let claimed = repo.claim_pending_with_lease(1, 0).await.unwrap();
    assert_eq!(claimed.len(), 1);
    let token = claimed[0].lease_token.as_ref().unwrap();

    // Worker attempts to mark as published after its lease has expired, but BEFORE any other worker has reclaimed it
    let res = repo.mark_published(&msg_id, token).await;
    assert!(
        res.is_err(),
        "completion must be rejected when lease is expired, even before peer claims"
    );

    // Worker attempts to mark as failed after lease expiry
    let fail_res = repo
        .mark_failed_with_max_retries(&msg_id, token, "some transient failure".into(), 5)
        .await;
    assert!(
        fail_res.is_err(),
        "mark_failed must be rejected when lease is expired"
    );

    // Message remains in flight (or eligible for recovery), not marked published or failed
    assert_eq!(repo.published_count().await.unwrap(), 0);
}

/// Faulty outbox repository wrapper that simulates database failure during recovery or acknowledgement.
#[derive(Debug)]
struct FaultyOutboxRepo {
    inner: Arc<SqliteBackend>,
    fail_recovery: bool,
    fail_mark_published: bool,
}

impl OutboxRepository for FaultyOutboxRepo {
    fn save<'a>(&'a self, message: OutboxMessage) -> rume::outbox::BoxFuture<'a, Result<(), OutboxError>> {
        self.inner.save(message)
    }

    fn fetch_pending<'a>(&'a self, limit: usize) -> rume::outbox::BoxFuture<'a, Result<Vec<OutboxMessage>, OutboxError>> {
        self.inner.fetch_pending(limit)
    }

    fn claim_pending_with_lease<'a>(
        &'a self,
        limit: usize,
        lease_duration_secs: u64,
    ) -> rume::outbox::BoxFuture<'a, Result<Vec<OutboxMessage>, OutboxError>> {
        self.inner.claim_pending_with_lease(limit, lease_duration_secs)
    }

    fn renew_outbox_lease<'a>(
        &'a self,
        id: &str,
        lease_token: &str,
        extension_secs: u64,
    ) -> rume::outbox::BoxFuture<'a, Result<(), OutboxError>> {
        self.inner.renew_outbox_lease(id, lease_token, extension_secs)
    }

    fn mark_published<'a>(
        &'a self,
        id: &str,
        lease_token: &str,
    ) -> rume::outbox::BoxFuture<'a, Result<(), OutboxError>> {
        if self.fail_mark_published {
            Box::pin(async move {
                Err(OutboxError::Repository("database connection drop during mark_published".into()))
            })
        } else {
            self.inner.mark_published(id, lease_token)
        }
    }

    fn mark_failed_with_max_retries<'a>(
        &'a self,
        id: &str,
        lease_token: &str,
        error: String,
        max_retries: u32,
    ) -> rume::outbox::BoxFuture<'a, Result<(), OutboxError>> {
        self.inner.mark_failed_with_max_retries(id, lease_token, error, max_retries)
    }

    fn recover_expired_claims<'a>(
        &'a self,
        lease_duration_secs: u64,
    ) -> rume::outbox::BoxFuture<'a, Result<usize, OutboxError>> {
        if self.fail_recovery {
            Box::pin(async move {
                Err(OutboxError::Repository("database connection failure during recovery".into()))
            })
        } else {
            self.inner.recover_expired_claims(lease_duration_secs)
        }
    }

    fn pending_count<'a>(&'a self) -> rume::outbox::BoxFuture<'a, Result<usize, OutboxError>> {
        self.inner.pending_count()
    }

    fn published_count<'a>(&'a self) -> rume::outbox::BoxFuture<'a, Result<usize, OutboxError>> {
        self.inner.published_count()
    }

    fn failed_count<'a>(&'a self) -> rume::outbox::BoxFuture<'a, Result<usize, OutboxError>> {
        self.inner.failed_count()
    }
}

#[tokio::test]
async fn outbox_relay_database_failure_during_recovery_is_returned_and_visible() {
    let inner = setup_real_outbox().await;
    let faulty = Arc::new(FaultyOutboxRepo {
        inner,
        fail_recovery: true,
        fail_mark_published: false,
    });

    let relay = OutboxRelay::new(Arc::clone(&faulty), |_msg: &OutboxMessage| async { Ok(()) });

    // Calling poll_and_dispatch must return the repository failure rather than logging and returning Ok(0)
    let res = relay.poll_and_dispatch(10).await;
    assert!(res.is_err(), "database failure during recovery must return Err");
    match res {
        Err(OutboxError::Repository(msg)) => {
            assert!(msg.contains("database connection failure during recovery"));
        }
        other => panic!("expected OutboxError::Repository, got {:?}", other),
    }
}

#[tokio::test]
async fn outbox_relay_failed_transition_is_distinguished_and_drain_fails() {
    let inner = setup_real_outbox().await;
    let event = OrderPaidEvent {
        order_id: "order_fail_trans_1".into(),
        amount_cents: 3500,
    };
    let msg = OutboxMessage::from_event(&event).unwrap();
    inner.save(msg).await.unwrap();

    let faulty = Arc::new(FaultyOutboxRepo {
        inner: Arc::clone(&inner),
        fail_recovery: false,
        fail_mark_published: true, // simulated failure during state transition / ack
    });

    let relay = OutboxRelay::new(Arc::clone(&faulty), |_msg: &OutboxMessage| async { Ok(()) });

    // poll_and_dispatch returns RelayPollResult with failed_transitions reported
    let res = relay.poll_and_dispatch(10).await.unwrap();
    assert_eq!(res.published, 0);
    assert_eq!(res.failed_transitions, 1);
    assert!(!res.is_success());
    assert_eq!(res.total_processed(), 1);

    // Save a new message to test try_drain encountering a failed transition
    let event2 = OrderPaidEvent {
        order_id: "order_fail_trans_2".into(),
        amount_cents: 4200,
    };
    let msg2 = OutboxMessage::from_event(&event2).unwrap();
    inner.save(msg2).await.unwrap();

    // try_drain immediately propagates the failed transition error rather than reporting success
    let drain_res = relay.try_drain(Duration::from_millis(500)).await;
    assert!(drain_res.is_err(), "try_drain must return Err on failed state transitions");
}

#[derive(Debug, Default)]
struct MockIdempotencyStore {
    records: std::sync::Mutex<std::collections::HashMap<String, rume::jobs::reliability::IdempotencyStatus>>,
}

impl rume::jobs::reliability::IdempotencyStore for MockIdempotencyStore {
    fn try_acquire(
        &self,
        key: &str,
        _ttl: Duration,
    ) -> rume::jobs::job::BoxFuture<'_, Result<rume::jobs::reliability::IdempotencyStatus, rume::jobs::error::JobError>> {
        let mut map = self.records.lock().unwrap();
        if let Some(status) = map.get(key) {
            let status = status.clone();
            Box::pin(async move { Ok(status) })
        } else {
            map.insert(key.to_string(), rume::jobs::reliability::IdempotencyStatus::InProgress);
            Box::pin(async move { Ok(rume::jobs::reliability::IdempotencyStatus::New) })
        }
    }

    fn try_acquire_with_token(
        &self,
        key: &str,
        _token: &str,
        ttl: Duration,
    ) -> rume::jobs::job::BoxFuture<'_, Result<rume::jobs::reliability::IdempotencyStatus, rume::jobs::error::JobError>> {
        self.try_acquire(key, ttl)
    }

    fn complete(
        &self,
        key: &str,
        payload: Option<Vec<u8>>,
    ) -> rume::jobs::job::BoxFuture<'_, Result<(), rume::jobs::error::JobError>> {
        let mut map = self.records.lock().unwrap();
        map.insert(
            key.to_string(),
            rume::jobs::reliability::IdempotencyStatus::Completed(payload),
        );
        Box::pin(async move { Ok(()) })
    }

    fn complete_with_token(
        &self,
        key: &str,
        _token: &str,
        payload: Option<Vec<u8>>,
    ) -> rume::jobs::job::BoxFuture<'_, Result<(), rume::jobs::error::JobError>> {
        self.complete(key, payload)
    }

    fn release(&self, key: &str) -> rume::jobs::job::BoxFuture<'_, Result<(), rume::jobs::error::JobError>> {
        let mut map = self.records.lock().unwrap();
        map.remove(key);
        Box::pin(async move { Ok(()) })
    }

    fn release_with_token(
        &self,
        key: &str,
        _token: &str,
    ) -> rume::jobs::job::BoxFuture<'_, Result<(), rume::jobs::error::JobError>> {
        self.release(key)
    }
}

#[tokio::test]
async fn duplicate_event_delivery_consumer_idempotency_prevents_duplicate_effects() {
    use rume::jobs::reliability::{IdempotencyStatus, IdempotencyStore, scoped_idempotency_key};

    let idempotency_store = Arc::new(MockIdempotencyStore::default());
    let business_effects_counter = Arc::new(AtomicUsize::new(0));

    // Simulated consumer event handler with consumer-side idempotency
    async fn handle_event(
        event_id: &str,
        tenant_id: &str,
        store: &MockIdempotencyStore,
        counter: &AtomicUsize,
    ) -> Result<&'static str, &'static str> {
        let key = scoped_idempotency_key(tenant_id, event_id);
        let status = store.try_acquire(&key, Duration::from_secs(60)).await.unwrap();

        match status {
            IdempotencyStatus::Completed(_) => {
                // Duplicate delivery! Skip business effects completely
                Ok("skipped_duplicate")
            }
            IdempotencyStatus::New => {
                // First delivery: execute critical business effect (e.g., balance charge, invoice generation)
                counter.fetch_add(500, Ordering::SeqCst);
                store.complete(&key, Some(b"processed".to_vec())).await.unwrap();
                Ok("processed")
            }
            IdempotencyStatus::InProgress => Err("concurrent_in_progress"),
        }
    }

    // 1. Initial event delivery: processes and executes business effect
    let res1 = handle_event(
        "evt_order_12345",
        "tenant_corp",
        &idempotency_store,
        &business_effects_counter,
    )
    .await
    .unwrap();
    assert_eq!(res1, "processed");
    assert_eq!(business_effects_counter.load(Ordering::SeqCst), 500);

    // 2. Duplicate event delivery arrives (e.g. at-least-once broker redelivery or retry)
    let res2 = handle_event(
        "evt_order_12345",
        "tenant_corp",
        &idempotency_store,
        &business_effects_counter,
    )
    .await
    .unwrap();
    assert_eq!(res2, "skipped_duplicate");

    // Invariant: Business effect must strictly execute exactly once!
    assert_eq!(
        business_effects_counter.load(Ordering::SeqCst),
        500,
        "consumer idempotency must guarantee duplicate event delivery does not duplicate business effects"
    );
}

#[tokio::test]
async fn sqlite_outbox_relay_heartbeat_renewal_keeps_long_running_dispatch_valid() {
    let repo = setup_real_outbox().await;
    let event = OrderPaidEvent {
        order_id: "order_relay_sqlite_hb".into(),
        amount_cents: 8900,
    };
    let msg = OutboxMessage::from_event(&event).unwrap();
    repo.save(msg).await.unwrap();

    let dispatched = Arc::new(AtomicUsize::new(0));
    let dispatched_clone = Arc::clone(&dispatched);

    // Relay with 1-second lease duration
    let config = OutboxRelayConfig {
        batch_size: 10,
        lease_duration_secs: 1,
        poll_interval: Duration::from_millis(50),
        max_retries: 3,
    };

    // Dispatcher runs for 1200ms (> initial 1s lease)
    let relay = OutboxRelay::new(Arc::clone(&repo), move |_msg: &OutboxMessage| {
        let count = Arc::clone(&dispatched_clone);
        async move {
            tokio::time::sleep(Duration::from_millis(1200)).await;
            count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    })
    .with_config(config);

    let res = relay.poll_and_dispatch(10).await.expect("dispatch");
    assert_eq!(res.published, 1);
    assert_eq!(res.failed_transitions, 0);
    assert_eq!(dispatched.load(Ordering::SeqCst), 1);
    assert_eq!(repo.pending_count().await.unwrap(), 0);
    assert_eq!(repo.published_count().await.unwrap(), 1);
}

#[derive(Debug)]
struct MockBareOutboxRepo;

impl OutboxRepository for MockBareOutboxRepo {
    fn save(&self, _msg: OutboxMessage) -> rume::outbox::BoxFuture<'_, Result<(), OutboxError>> {
        Box::pin(async move { Ok(()) })
    }
    fn fetch_pending(&self, _limit: usize) -> rume::outbox::BoxFuture<'_, Result<Vec<OutboxMessage>, OutboxError>> {
        Box::pin(async move { Ok(vec![]) })
    }
    fn claim_pending_with_lease(&self, _limit: usize, _secs: u64) -> rume::outbox::BoxFuture<'_, Result<Vec<OutboxMessage>, OutboxError>> {
        Box::pin(async move { Ok(vec![]) })
    }
    fn mark_published(&self, _id: &str, _token: &str) -> rume::outbox::BoxFuture<'_, Result<(), OutboxError>> {
        Box::pin(async move { Ok(()) })
    }
    fn pending_count(&self) -> rume::outbox::BoxFuture<'_, Result<usize, OutboxError>> {
        Box::pin(async move { Ok(0) })
    }
    fn published_count(&self) -> rume::outbox::BoxFuture<'_, Result<usize, OutboxError>> {
        Box::pin(async move { Ok(0) })
    }
}

#[tokio::test]
async fn test_outbox_repository_unsupported_renewal_default_returns_error() {
    let repo = MockBareOutboxRepo;
    let res = repo.renew_outbox_lease("msg-bare-1", "token-xyz", 30).await;
    match res {
        Err(OutboxError::Unsupported(msg)) => {
            assert!(msg.contains("msg-bare-1"));
        }
        other => panic!("expected OutboxError::Unsupported, got: {other:?}"),
    }
}

#[tokio::test]
async fn database_pool_sqlite_outbox_relay_heartbeat_renewal_keeps_long_running_dispatch_valid() {
    let backend = SqliteBackend::in_memory().await.expect("sqlite in memory");
    backend.init_outbox_table().await.expect("init outbox table");
    let pool = Arc::new(rume::persistence::pool::DatabasePool::new(
        backend,
        rume::persistence::config::DatabaseConfig::default(),
    ));

    let event = OrderPaidEvent {
        order_id: "order_relay_pool_hb".into(),
        amount_cents: 9900,
    };
    let msg = OutboxMessage::from_event(&event).unwrap();
    pool.save(msg).await.unwrap();

    let dispatched = Arc::new(AtomicUsize::new(0));
    let dispatched_clone = Arc::clone(&dispatched);

    let config = OutboxRelayConfig {
        batch_size: 10,
        lease_duration_secs: 1,
        poll_interval: Duration::from_millis(50),
        max_retries: 3,
    };

    let relay = OutboxRelay::new(Arc::clone(&pool), move |_msg: &OutboxMessage| {
        let count = Arc::clone(&dispatched_clone);
        async move {
            tokio::time::sleep(Duration::from_millis(1200)).await;
            count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    })
    .with_config(config);

    let res = relay.poll_and_dispatch(10).await.expect("dispatch");
    assert_eq!(res.published, 1);
    assert_eq!(res.failed_transitions, 0);
    assert_eq!(dispatched.load(Ordering::SeqCst), 1);
    assert_eq!(pool.pending_count().await.unwrap(), 0);
    assert_eq!(pool.published_count().await.unwrap(), 1);
}
