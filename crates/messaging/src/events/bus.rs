//! In-process typed event bus with synchronous dispatch and asynchronous background delivery.

use futures_util::FutureExt;
use std::any::{Any, TypeId};
use std::collections::BTreeMap;
use std::fmt::Debug;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, RwLock};
use tracing::Instrument;

use crate::events::error::EventError;
use crate::events::event::{BoxFuture, ErasedEventListener, Event, EventHandler};

/// An in-process event bus enabling decoupled inter-module communication.
#[derive(Clone, Default)]
pub struct EventBus {
    subscribers: Arc<RwLock<BTreeMap<TypeId, Vec<ErasedEventListener>>>>,
}

impl Debug for EventBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let count = self.subscribers.read().map(|s| s.len()).unwrap_or(0);
        f.debug_struct("EventBus")
            .field("subscribed_event_types", &count)
            .finish()
    }
}

impl EventBus {
    /// Creates a new empty event bus.
    pub fn new() -> Self {
        Self {
            subscribers: Arc::new(RwLock::new(BTreeMap::new())),
        }
    }

    /// Subscribes a typed [`EventHandler`] to an event type.
    pub fn subscribe<E, H>(&self, handler: H)
    where
        E: Event,
        H: EventHandler<E>,
    {
        let handler = Arc::new(handler);
        let erased: ErasedEventListener = Arc::new(move |any_arc: Arc<dyn Any + Send + Sync>| {
            let handler = Arc::clone(&handler);
            Box::pin(async move {
                if let Some(event) = any_arc.downcast_ref::<E>() {
                    handler.handle(event).await
                } else {
                    Err(EventError::Bus(format!(
                        "failed to downcast event to '{}'",
                        E::TYPE
                    )))
                }
            })
        });

        if let Ok(mut subs) = self.subscribers.write() {
            subs.entry(TypeId::of::<E>()).or_default().push(erased);
        }
    }

    /// Subscribes an asynchronous closure or function to an event type.
    pub fn subscribe_fn<E, F, Fut>(&self, func: F)
    where
        E: Event,
        F: Fn(&E) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<(), EventError>> + Send + 'static,
    {
        struct FnHandler<F>(F);
        impl<E, F, Fut> EventHandler<E> for FnHandler<F>
        where
            E: Event,
            F: Fn(&E) -> Fut + Send + Sync + 'static,
            Fut: std::future::Future<Output = Result<(), EventError>> + Send + 'static,
        {
            fn handle(&self, event: &E) -> BoxFuture<'static, Result<(), EventError>> {
                Box::pin((self.0)(event))
            }
        }

        self.subscribe(FnHandler(func));
    }

    /// Returns the number of registered listeners for an event type.
    pub fn listener_count<E: Event>(&self) -> usize {
        if let Ok(subs) = self.subscribers.read() {
            subs.get(&TypeId::of::<E>()).map(|l| l.len()).unwrap_or(0)
        } else {
            0
        }
    }

    /// Dispatches an event synchronously, awaiting all registered listeners before returning.
    ///
    /// If any listener fails, returns [`EventError`].
    pub async fn publish_sync<E: Event>(&self, event: &E) -> Result<usize, EventError> {
        let listeners = {
            let subs = self
                .subscribers
                .read()
                .map_err(|e| EventError::Bus(e.to_string()))?;
            subs.get(&TypeId::of::<E>()).cloned().unwrap_or_default()
        };

        let count = listeners.len();
        let event_arc: Arc<dyn Any + Send + Sync> = Arc::new(event.clone());
        let current_tc = rume_observability::TraceContext::current();
        let span = tracing::info_span!(
            "event.publish_sync",
            event.type = E::TYPE,
            traceparent = current_tc
                .as_ref()
                .map(|tc| tc.traceparent.as_str())
                .unwrap_or(""),
        );

        let run_listeners = async move {
            for listener in listeners {
                // Why: wrap the synchronous closure invocation itself in
                // `catch_unwind` as well as polling the returned future, so a
                // listener that panics before returning its `BoxFuture` cannot
                // unwind `publish_sync` or abort remaining listeners.
                let fut = match std::panic::catch_unwind(AssertUnwindSafe(|| {
                    listener(Arc::clone(&event_arc))
                })) {
                    Ok(fut) => fut,
                    Err(_) => {
                        return Err(EventError::ListenerError {
                            event_type: E::TYPE,
                            message: "event listener panicked".into(),
                        });
                    }
                };
                match AssertUnwindSafe(fut).catch_unwind().await {
                    Ok(Ok(())) => {}
                    Ok(Err(err)) => return Err(err),
                    Err(_) => {
                        return Err(EventError::ListenerError {
                            event_type: E::TYPE,
                            message: "event listener panicked".into(),
                        });
                    }
                }
            }
            Ok(count)
        };

        if let Some(tc) = current_tc {
            // Why: pass `tc` directly into `scope_future(span, ...)` so `OpenTelemetryLayer`
            // creates a child `event.publish_sync` span under `tc` (passing `tc.new_child()` would
            // reference a nonexistent parent span ID), while without an OTel layer `with_context`
            // keeps `OtelContext::current()` populated for `TraceContext::current()` in listeners.
            tc.scope_future(span, run_listeners).await
        } else {
            run_listeners.instrument(span).await
        }
    }

    /// Dispatches an event asynchronously in a background task, returning immediately to the caller.
    ///
    /// Returns the number of listeners that will be notified.
    pub fn publish_async<E: Event>(&self, event: E) -> usize {
        let listeners = {
            if let Ok(subs) = self.subscribers.read() {
                subs.get(&TypeId::of::<E>()).cloned().unwrap_or_default()
            } else {
                Vec::new()
            }
        };

        let count = listeners.len();
        if count > 0 {
            let event_arc: Arc<dyn Any + Send + Sync> = Arc::new(event);
            let current_tc = rume_observability::TraceContext::current();
            let span = tracing::info_span!(
                "event.publish_async",
                event.type = E::TYPE,
                traceparent = current_tc.as_ref().map(|tc| tc.traceparent.as_str()).unwrap_or(""),
            );
            let run_listeners = async move {
                for listener in listeners {
                    let fut = match std::panic::catch_unwind(AssertUnwindSafe(|| {
                        listener(Arc::clone(&event_arc))
                    })) {
                        Ok(fut) => fut,
                        Err(panic_payload) => {
                            tracing::error!(
                                "async event listener panicked synchronously: {panic_payload:?}"
                            );
                            continue;
                        }
                    };
                    match AssertUnwindSafe(fut).catch_unwind().await {
                        Ok(Ok(())) => {}
                        Ok(Err(err)) => {
                            tracing::error!("async event listener returned error: {err}");
                        }
                        Err(panic_payload) => {
                            tracing::error!("async event listener panicked: {panic_payload:?}");
                        }
                    }
                }
            };
            if let Some(tc) = current_tc {
                // Why: pass `tc` directly to `scope_future(span, ...)` so `OpenTelemetryLayer`
                // parents `event.publish_async` under `tc` across the `tokio::spawn` boundary
                // without inventing a phantom intermediate `span_id`, and non-OTel subscribers
                // still retain `OtelContext::current()` fallback inside spawned listeners.
                tokio::spawn(tc.scope_future(span, run_listeners));
            } else {
                tokio::spawn(run_listeners.instrument(span));
            }
        }

        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry::trace::TracerProvider as _;
    use opentelemetry_sdk::trace::SdkTracerProvider;
    use serde::{Deserialize, Serialize};
    use std::sync::Mutex;
    use tracing_subscriber::layer::SubscriberExt as _;

    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct TestSyncEvent {
        id: String,
    }

    impl Event for TestSyncEvent {
        const TYPE: &'static str = "test.sync_event";
    }

    static SUBSCRIBER_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[tokio::test]
    async fn event_bus_publish_sync_creates_span_and_propagates_trace_context() {
        // Why: `tracing-core` caches per-callsite `Interest` globally across threads.
        // Concurrent tests without an active subscriber can cache `Interest::never()` on
        // `event.publish_sync` or `test.caller`, so serialize subscriber-override tests
        // and rebuild the interest cache after installing each test subscriber.
        let _sub_lock = SUBSCRIBER_TEST_LOCK.lock().await;

        let parent_trace_id = "4bf92f3577b34da6a3ce929d0e0e4736";
        let parent_span_id = "00f067aa0ba902b7";
        let parent_tc = rume_observability::TraceContext::from_traceparent(format!(
            "00-{parent_trace_id}-{parent_span_id}-01"
        ));

        // 1. With OpenTelemetryLayer active: listener observes preserved trace_id and child span_id
        let provider = SdkTracerProvider::builder().build();
        let tracer = provider.tracer("event-bus-test");
        let subscriber =
            tracing_subscriber::registry().with(tracing_opentelemetry::layer().with_tracer(tracer));
        let _guard = tracing::subscriber::set_default(subscriber);
        tracing::callsite::rebuild_interest_cache();

        let bus = EventBus::new();
        let seen_otel = Arc::new(Mutex::new(None::<rume_observability::TraceContext>));
        let seen_otel_clone = Arc::clone(&seen_otel);
        bus.subscribe_fn::<TestSyncEvent, _, _>(move |_evt| {
            let seen = Arc::clone(&seen_otel_clone);
            async move {
                *seen.lock().unwrap() = rume_observability::TraceContext::current();
                Ok(())
            }
        });

        let caller_tc = Arc::new(Mutex::new(None::<rume_observability::TraceContext>));
        let caller_tc_clone = Arc::clone(&caller_tc);
        let root_span = tracing::info_span!("test.caller");
        parent_tc
            .scope_future(root_span, async move {
                *caller_tc_clone.lock().unwrap() = rume_observability::TraceContext::current();
                bus.publish_sync(&TestSyncEvent { id: "evt-1".into() })
                    .await
                    .unwrap();
            })
            .await;

        let caller_tc = caller_tc
            .lock()
            .unwrap()
            .clone()
            .expect("caller must observe TraceContext::current()");
        let listener_tc = seen_otel
            .lock()
            .unwrap()
            .clone()
            .expect("listener must observe TraceContext::current()");
        assert_eq!(listener_tc.trace_id(), parent_trace_id);
        assert_ne!(
            listener_tc.span_id(),
            caller_tc.span_id(),
            "event.publish_sync span must allocate a child span_id under OpenTelemetryLayer"
        );

        // 2. Without OpenTelemetryLayer: fallback preserves TraceContext::current() trace_id
        drop(_guard);
        let no_otel_sub = tracing_subscriber::registry();
        let _no_otel_guard = tracing::subscriber::set_default(no_otel_sub);
        tracing::callsite::rebuild_interest_cache();
        let bus_no_otel = EventBus::new();
        let seen_no_otel = Arc::new(Mutex::new(None::<rume_observability::TraceContext>));
        let seen_no_otel_clone = Arc::clone(&seen_no_otel);
        bus_no_otel.subscribe_fn::<TestSyncEvent, _, _>(move |_evt| {
            let seen = Arc::clone(&seen_no_otel_clone);
            async move {
                *seen.lock().unwrap() = rume_observability::TraceContext::current();
                Ok(())
            }
        });

        let fallback_span = tracing::info_span!("test.caller_no_otel");
        parent_tc
            .scope_future(fallback_span, async move {
                bus_no_otel
                    .publish_sync(&TestSyncEvent { id: "evt-2".into() })
                    .await
                    .unwrap();
            })
            .await;
        let fallback_tc = seen_no_otel
            .lock()
            .unwrap()
            .clone()
            .expect("listener must observe TraceContext::current() without OTel layer");
        assert_eq!(fallback_tc.trace_id(), parent_trace_id);
    }

    #[tokio::test]
    async fn sync_panicking_listener_is_caught_in_publish_sync_and_publish_async() {
        let bus = EventBus::new();
        let second_done = Arc::new(tokio::sync::Notify::new());
        let second_done_clone = Arc::clone(&second_done);

        bus.subscribe_fn::<TestSyncEvent, _, _>(
            |_evt| -> std::future::Ready<Result<(), EventError>> {
                panic!("sync panic before returning future");
            },
        );
        bus.subscribe_fn::<TestSyncEvent, _, _>(move |_evt| {
            let notify = Arc::clone(&second_done_clone);
            async move {
                notify.notify_one();
                Ok(())
            }
        });

        let err = bus
            .publish_sync(&TestSyncEvent { id: "s1".into() })
            .await
            .unwrap_err();
        assert!(matches!(err, EventError::ListenerError { .. }));

        let notified = bus.publish_async(TestSyncEvent { id: "a1".into() });
        assert_eq!(notified, 2);
        tokio::time::timeout(std::time::Duration::from_secs(2), second_done.notified())
            .await
            .expect(
                "second healthy listener must still run in publish_async after first listener panics synchronously",
            );
    }
}
