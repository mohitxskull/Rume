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
#[cfg(feature = "mysql")]
mod mysql;
#[cfg(feature = "postgres")]
mod postgres;
pub mod relay;
pub mod repository;
#[cfg(feature = "sqlite")]
mod sqlite;

pub use error::OutboxError;
pub use message::{OutboxMessage, OutboxStatus};
pub use relay::{
    OutboxRelay, OutboxRelayConfig, OutboxSweeperConfig, RelayHealthCheck, RelayPollResult,
    RelayStatus,
};
pub use repository::{BoxFuture, OutboxRepository, TransactionalOutbox};

/// Maximum exponential retry backoff applied to failed outbox messages (1 hour).
#[cfg(any(feature = "postgres", feature = "mysql", feature = "sqlite", test))]
pub(crate) const MAX_OUTBOX_BACKOFF_SECS: i64 = 3600;

/// Computes `(next_retry_count, next_status, next_backoff_secs)` for an outbox
/// failure without overflowing or panicking on high `retry_count` / `max_retries`.
///
/// Why: `2u64.pow(retry_count - 1) as i64` wraps negative at `retry_count == 64`
/// and panics in debug at `>= 65`. Capping the exponent and ceiling at
/// `MAX_OUTBOX_BACKOFF_SECS` and returning `None` (SQL `NULL`) when `status == "failed"`
/// keeps retry scheduling bounded and monotonic.
#[cfg(any(feature = "postgres", feature = "mysql", feature = "sqlite", test))]
pub(crate) fn compute_outbox_retry_state(
    current_retry_count: i32,
    max_retries: u32,
) -> (i32, &'static str, Option<i64>) {
    let next_retry = current_retry_count.saturating_add(1);
    if next_retry.max(0) as u32 >= max_retries {
        (next_retry, "failed", None)
    } else {
        let exp = (next_retry.saturating_sub(1).max(0) as u32).min(30);
        let backoff_secs = (1_i64 << exp).min(MAX_OUTBOX_BACKOFF_SECS);
        (next_retry, "pending", Some(backoff_secs))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outbox_backoff_caps_without_overflow_for_high_retry_counts() {
        assert_eq!(compute_outbox_retry_state(0, 5), (1, "pending", Some(1)));
        assert_eq!(compute_outbox_retry_state(1, 5), (2, "pending", Some(2)));
        assert_eq!(
            compute_outbox_retry_state(63, 100),
            (64, "pending", Some(MAX_OUTBOX_BACKOFF_SECS))
        );
        assert_eq!(
            compute_outbox_retry_state(64, 100),
            (65, "pending", Some(MAX_OUTBOX_BACKOFF_SECS))
        );
        assert_eq!(compute_outbox_retry_state(4, 5), (5, "failed", None));
    }
}
