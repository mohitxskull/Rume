use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use rume::events::Event;
use rume::outbox::{
    MemoryOutboxRepository, OutboxError, OutboxMessage, OutboxRelay, OutboxRepository, OutboxStatus,
};
use serde::{Deserialize, Serialize};

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
    let repo = Arc::new(MemoryOutboxRepository::new());
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
    let repo = Arc::new(MemoryOutboxRepository::new());

    let event = OrderPaidEvent {
        order_id: "failing_ord".into(),
        amount_cents: 500,
    };
    let msg = OutboxMessage::from_event(&event).unwrap();
    repo.save(msg).await.unwrap();

    let relay = OutboxRelay::new(Arc::clone(&repo), |_msg| async move {
        Err(OutboxError::Dispatch("kafka cluster unreachable".into()))
    });

    // Run 4 failures -> still Pending, retry_count = 4
    for _ in 0..4 {
        let _ = relay.poll_and_dispatch(1).await;
    }

    let pending = repo.fetch_pending(1).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].retry_count, 4);
    assert_eq!(pending[0].status, OutboxStatus::Pending);

    // 5th failure -> status transitions to Failed
    let _ = relay.poll_and_dispatch(1).await;
    let pending_after = repo.fetch_pending(1).await.unwrap();
    assert_eq!(pending_after.len(), 0); // No longer returned as Pending

    assert_eq!(repo.pending_count().await.unwrap(), 0);
}

#[tokio::test]
async fn outbox_relay_graceful_drain_on_shutdown() {
    let repo = Arc::new(MemoryOutboxRepository::new());
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
    let repo = Arc::new(MemoryOutboxRepository::new());

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
