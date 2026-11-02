use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use rume::events::{Event, EventBus, EventError, EventHandler};
use tokio::time::sleep;

#[derive(Debug, Clone, PartialEq, Eq)]
struct UserRegisteredEvent {
    user_id: String,
    email: String,
}

impl Event for UserRegisteredEvent {
    const TYPE: &'static str = "user.registered";
}

struct WelcomeNotificationListener {
    notifications_sent: Arc<AtomicUsize>,
}

impl EventHandler<UserRegisteredEvent> for WelcomeNotificationListener {
    fn handle(
        &self,
        _event: &UserRegisteredEvent,
    ) -> rume::events::BoxFuture<'static, Result<(), EventError>> {
        let sent = Arc::clone(&self.notifications_sent);
        Box::pin(async move {
            sent.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
}

#[tokio::test]
async fn synchronous_event_dispatch_to_multiple_listeners() {
    let bus = EventBus::new();

    let notification_counter = Arc::new(AtomicUsize::new(0));
    let audit_counter = Arc::new(AtomicUsize::new(0));

    // 1. Subscribe typed handler struct
    bus.subscribe(WelcomeNotificationListener {
        notifications_sent: Arc::clone(&notification_counter),
    });

    // 2. Subscribe async closure
    let audit_clone = Arc::clone(&audit_counter);
    bus.subscribe_fn(move |event: &UserRegisteredEvent| {
        let audit = Arc::clone(&audit_clone);
        let id = event.user_id.clone();
        async move {
            assert_eq!(id, "user_42");
            audit.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    });

    assert_eq!(bus.listener_count::<UserRegisteredEvent>(), 2);

    let event = UserRegisteredEvent {
        user_id: "user_42".into(),
        email: "alice@example.com".into(),
    };

    let delivered = bus.publish_sync(&event).await.unwrap();
    assert_eq!(delivered, 2);

    assert_eq!(notification_counter.load(Ordering::SeqCst), 1);
    assert_eq!(audit_counter.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn synchronous_dispatch_propagates_listener_error() {
    let bus = EventBus::new();

    bus.subscribe_fn(|_event: &UserRegisteredEvent| async move {
        Err(EventError::ListenerError {
            event_type: UserRegisteredEvent::TYPE,
            message: "database write conflict".into(),
        })
    });

    let event = UserRegisteredEvent {
        user_id: "user_err".into(),
        email: "fail@example.com".into(),
    };

    let result = bus.publish_sync(&event).await;
    match result {
        Err(EventError::ListenerError { message, .. }) => {
            assert!(message.contains("database write conflict"));
        }
        _ => panic!("expected ListenerError"),
    }
}

#[tokio::test]
async fn asynchronous_event_delivery_unblocks_caller() {
    let bus = EventBus::new();
    let async_counter = Arc::new(AtomicUsize::new(0));

    let counter_clone = Arc::clone(&async_counter);
    bus.subscribe_fn(move |_event: &UserRegisteredEvent| {
        let cnt = Arc::clone(&counter_clone);
        async move {
            sleep(Duration::from_millis(20)).await;
            cnt.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    });

    let event = UserRegisteredEvent {
        user_id: "user_async".into(),
        email: "async@example.com".into(),
    };

    // publish_async returns immediately
    let scheduled = bus.publish_async(event);
    assert_eq!(scheduled, 1);
    assert_eq!(async_counter.load(Ordering::SeqCst), 0);

    // Wait for async task to complete
    sleep(Duration::from_millis(50)).await;
    assert_eq!(async_counter.load(Ordering::SeqCst), 1);
}

#[derive(Debug, Clone)]
struct OrderPlacedEvent {
    order_id: String,
    total_cents: u64,
}

impl Event for OrderPlacedEvent {
    const TYPE: &'static str = "order.placed";
}

#[tokio::test]
async fn decoupled_inter_module_events() {
    let bus = Arc::new(EventBus::new());
    let total_revenue = Arc::new(AtomicUsize::new(0));

    // Analytics module subscribes to OrderPlacedEvent
    let rev_clone = Arc::clone(&total_revenue);
    bus.subscribe_fn(move |ev: &OrderPlacedEvent| {
        let rev = Arc::clone(&rev_clone);
        let cents = ev.total_cents as usize;
        let _id = &ev.order_id;
        async move {
            rev.fetch_add(cents, Ordering::SeqCst);
            Ok(())
        }
    });

    // Orders module publishes an order
    bus.publish_sync(&OrderPlacedEvent {
        order_id: "ord_1".into(),
        total_cents: 2500,
    })
    .await
    .unwrap();

    bus.publish_sync(&OrderPlacedEvent {
        order_id: "ord_2".into(),
        total_cents: 7500,
    })
    .await
    .unwrap();

    // Verify analytics received both events without orders module knowing about analytics
    assert_eq!(total_revenue.load(Ordering::SeqCst), 10000);
}
