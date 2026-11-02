//! Typed background job declarations, queues, retries, and worker runners.

pub mod error;
pub mod job;
#[cfg(feature = "postgres")]
pub mod postgres;
pub mod queue;
pub mod reliability;
pub mod retry;
pub mod worker;

pub use error::JobError;
pub use job::{BoxFuture, Job, JobContext, JobHandler, QueuedJob};
#[cfg(feature = "postgres")]
pub use postgres::PostgresJobQueue;
pub use queue::{JobQueue, MemoryJobQueue};
pub use reliability::{
    HeartbeatRegistry, IdempotencyStatus, IdempotencyStore, MemoryHeartbeatRegistry,
    MemoryIdempotencyStore,
};
pub use retry::RetryPolicy;
pub use worker::{JobRegistry, JobWorker};
