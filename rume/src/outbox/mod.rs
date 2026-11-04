//! Transactional Outbox pattern for reliable domain event and message publication.
//!
//! # Delivery Guarantees & Ordering Contract
//!
//! - **Transactional Atomicity**: Atomicity is scoped strictly to the local database transaction.
//!   Outbox messages are inserted within the same ACID transaction as domain entity state changes.
//! - **At-Least-Once Delivery**: External dispatch via [`OutboxRelay`] guarantees at-least-once
//!   publication. If a network failure or process crash occurs between dispatch and state acknowledgment,
//!   the unacknowledged message will be reclaimed and re-dispatched upon lease expiration.
//! - **Ordering Contract**: Relay workers provide **best-effort FIFO** ordering ordered by
//!   persisted timestamp and message ID (`created_at ASC, id ASC`). Under concurrent relay execution,
//!   worker failovers, or message retry backoff, delivery ordering is non-causal. Applications requiring
//!   strict causal or partition-level sequence ordering must embed monotonic version numbers or
//!   aggregate sequence IDs directly within event payloads.
//! - **Consumer Idempotency**: Downstream message handlers must be idempotent or employ
//!   deduplication stores (such as [`crate::jobs::IdempotencyStore`]) to safely handle redeliveries.

pub mod error;
pub mod message;
pub mod relay;
pub mod repository;

pub use error::OutboxError;
pub use message::{OutboxMessage, OutboxStatus};
pub use relay::{
    OutboxRelay, OutboxRelayConfig, OutboxSweeperConfig, RelayHealthCheck, RelayPollResult,
    RelayStatus,
};
pub use repository::{BoxFuture, OutboxRepository, TransactionalOutbox};
