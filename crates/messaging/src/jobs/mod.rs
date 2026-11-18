//! Typed background job declarations, queues, retries, and worker runners.

pub mod error;
pub mod job;
#[cfg(feature = "postgres")]
pub mod postgres;
pub mod queue;
#[cfg(feature = "redis")]
pub mod redis;
pub mod reliability;
pub mod retry;
pub mod worker;

#[cfg(feature = "redis")]
pub use self::redis::{RedisHeartbeatRegistry, RedisIdempotencyStore};
pub use error::JobError;
pub use job::{BoxFuture, Job, JobContext, JobHandler, QueuedJob};
#[cfg(feature = "postgres")]
pub use postgres::PostgresJobQueue;
pub use queue::JobQueue;
pub use reliability::{HeartbeatRegistry, IdempotencyStatus, IdempotencyStore};
pub use retry::RetryPolicy;
pub use worker::{JobRegistry, JobWorker, WorkerHandle, WorkerStatus};
