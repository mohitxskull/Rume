//! Typed inter-module domain events and application event bus.

pub mod bus;
pub mod error;
pub mod event;

pub use bus::EventBus;
pub use error::EventError;
pub use event::{BoxFuture, Event, EventHandler};
