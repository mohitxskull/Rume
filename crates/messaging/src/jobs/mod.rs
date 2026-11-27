//! Typed background job declarations, queues, retries, and worker runners.

pub mod batch;
pub mod error;
pub mod job;
#[cfg(feature = "mysql")]
pub mod mysql;
#[cfg(feature = "postgres")]
pub mod postgres;
pub mod queue;
#[cfg(feature = "redis")]
pub mod redis;
pub mod reliability;
pub mod retry;
#[cfg(feature = "sqlite")]
pub mod sqlite;
pub mod worker;

#[cfg(feature = "redis")]
pub use self::redis::{RedisHeartbeatRegistry, RedisIdempotencyStore};
#[cfg(any(feature = "sqlite", feature = "postgres", feature = "mysql"))]
pub use batch::SqlBatchStore;
pub use batch::{BatchBuilder, BatchId, BatchStatus, BatchStore, InMemoryBatchStore, JobBatch};
pub use error::JobError;
pub use job::{BoxFuture, Job, JobContext, JobHandler, QueuedJob, RawJob};
#[cfg(feature = "mysql")]
pub use mysql::MysqlJobQueue;
#[cfg(feature = "postgres")]
pub use postgres::PostgresJobQueue;
pub use queue::JobQueue;
pub use reliability::{HeartbeatRegistry, IdempotencyStatus, IdempotencyStore};
pub use retry::RetryPolicy;
#[cfg(feature = "sqlite")]
pub use sqlite::SqliteJobQueue;
pub use worker::{JobRegistry, JobWorker, WorkerHandle, WorkerStatus};
