//! In-process typed event bus with synchronous dispatch and asynchronous background delivery.

use std::any::{Any, TypeId};
use std::collections::BTreeMap;
use std::fmt::Debug;
use std::sync::{Arc, RwLock};

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

        for listener in listeners {
            listener(Arc::clone(&event_arc)).await?;
        }

        Ok(count)
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
            tokio::spawn(async move {
                for listener in listeners {
                    let _ = listener(Arc::clone(&event_arc)).await;
                }
            });
        }

        count
    }
}
