//! Event declarations, contracts, and handler interfaces.

use std::any::Any;
use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::events::error::EventError;

/// Pinned boxed future type alias for async event handler operations.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Trait implemented by typed domain and application events.
pub trait Event: Debug + Clone + Send + Sync + 'static {
    /// Canonical event type identifier (e.g. "order.placed", "user.registered").
    const TYPE: &'static str;
}

/// Asynchronous event handler listening for a specific event type.
pub trait EventHandler<E: Event>: Send + Sync + 'static {
    /// Handles the received event.
    fn handle(&self, event: &E) -> BoxFuture<'static, Result<(), EventError>>;
}

/// Dynamic type-erased listener wrapper used internally by the [`EventBus`](crate::events::bus::EventBus).
pub(crate) type ErasedEventListener = Arc<
    dyn Fn(Arc<dyn Any + Send + Sync>) -> BoxFuture<'static, Result<(), EventError>> + Send + Sync,
>;
