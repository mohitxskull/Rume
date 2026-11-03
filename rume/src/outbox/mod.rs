//! Transactional Outbox pattern for guaranteed, atomic event and message publication.

pub mod error;
pub mod message;
pub mod relay;
pub mod repository;

pub use error::OutboxError;
pub use message::{OutboxMessage, OutboxStatus};
pub use relay::{OutboxRelay, OutboxRelayConfig};
pub use repository::{BoxFuture, OutboxRepository, TransactionalOutbox};
