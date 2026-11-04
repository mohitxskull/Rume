//! Typed inter-module domain events and application event bus.
//!
//! Subscriptions and event dispatches are keyed by [`std::any::TypeId`] for compile-time
//! and runtime type isolation. Event handler closures downcast through [`std::any::Any`],
//! failing closed with [`EventError::Bus`] if type downcasting fails.

pub mod bus;
pub mod error;
pub mod event;

pub use bus::EventBus;
pub use error::EventError;
pub use event::{BoxFuture, Event, EventHandler};
