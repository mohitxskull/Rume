//! MySQL-backed durable background job queue using SQLx.

use std::fmt::Debug;
use std::time::Duration;

use sqlx::MySqlPool;

use crate::jobs::error::JobError;
use crate::jobs::job::{BoxFuture, Job, QueuedJob};
use crate::jobs::queue::JobQueue;

/// MySQL-backed implementation of [`JobQueue`].
#[derive(Clone)]
pub struct MysqlJobQueue {
    pool: MySqlPool,
    default_lease_duration: Duration,
}

impl Debug for MysqlJobQueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MysqlJobQueue")
            .field("default_lease_duration", &self.default_lease_duration)
            .finish()
    }
}

impl MysqlJobQueue {
    /// Schema DDL for durable background jobs in MySQL.
    pub const SCHEMA: &'static str = rume_data::sql::MYSQL_JOBS_SCHEMA;

    /// Creates a new MySQL job queue with a 60-second default lease duration.
    pub fn new(pool: MySqlPool) -> Self {
        Self {
            pool,
            default_lease_duration: Duration::from_secs(60),
        }
    }

    /// Sets the worker claim lease duration.
    pub fn with_lease_duration(mut self, duration: Duration) -> Self {
        self.default_lease_duration = duration;
        self
    }

    /// Returns a reference to the underlying MySQL connection pool.
    pub fn pool(&self) -> &MySqlPool {
        &self.pool
    }

    /// Initializes the `rume_jobs` table and indexes if not already present.
    pub async fn init_schema(&self) -> Result<(), JobError> {
        todo!("Task 02: implement MysqlJobQueue::init_schema")
    }
}

impl JobQueue for MysqlJobQueue {
    fn enqueue_delayed_with_trace<J: Job>(
        &self,
        _job: J,
        _delay: Duration,
        _trace_context: Option<String>,
    ) -> BoxFuture<'_, Result<String, JobError>> {
        todo!("Task 02: implement MysqlJobQueue::enqueue_delayed_with_trace")
    }

    fn dequeue_for(&self, _worker_id: &str) -> BoxFuture<'_, Result<Option<QueuedJob>, JobError>> {
        todo!("Task 02: implement MysqlJobQueue::dequeue_for")
    }

    fn dequeue_from(
        &self,
        _worker_id: &str,
        _queues: Option<&[&str]>,
    ) -> BoxFuture<'_, Result<Option<QueuedJob>, JobError>> {
        todo!("Task 02: implement MysqlJobQueue::dequeue_from")
    }

    fn reclaim_abandoned_jobs(
        &self,
        _dead_workers: &[String],
    ) -> BoxFuture<'_, Result<usize, JobError>> {
        todo!("Task 02: implement MysqlJobQueue::reclaim_abandoned_jobs")
    }

    fn ack(&self, _job_id: &str, _lease_token: &str) -> BoxFuture<'_, Result<(), JobError>> {
        todo!("Task 02: implement MysqlJobQueue::ack")
    }

    fn nack(
        &self,
        _job: QueuedJob,
        _lease_token: &str,
        _delay: Duration,
    ) -> BoxFuture<'_, Result<(), JobError>> {
        todo!("Task 02: implement MysqlJobQueue::nack")
    }

    fn defer(
        &self,
        _job: QueuedJob,
        _lease_token: &str,
        _delay: Duration,
    ) -> BoxFuture<'_, Result<(), JobError>> {
        todo!("Task 02: implement MysqlJobQueue::defer")
    }

    fn dead_letter(
        &self,
        _job: QueuedJob,
        _lease_token: &str,
        _reason: String,
    ) -> BoxFuture<'_, Result<(), JobError>> {
        todo!("Task 02: implement MysqlJobQueue::dead_letter")
    }

    fn renew_lease(
        &self,
        _job_id: &str,
        _lease_token: &str,
        _extension: Duration,
    ) -> BoxFuture<'_, Result<(), JobError>> {
        todo!("Task 02: implement MysqlJobQueue::renew_lease")
    }

    fn pending_count(&self) -> BoxFuture<'_, Result<usize, JobError>> {
        todo!("Task 02: implement MysqlJobQueue::pending_count")
    }

    fn dead_letter_count(&self) -> BoxFuture<'_, Result<usize, JobError>> {
        todo!("Task 02: implement MysqlJobQueue::dead_letter_count")
    }

    fn list_dead_letters(&self, _limit: usize) -> BoxFuture<'_, Result<Vec<QueuedJob>, JobError>> {
        todo!("Task 02: implement MysqlJobQueue::list_dead_letters")
    }

    fn purge_completed(&self, _retention: Duration) -> BoxFuture<'_, Result<u64, JobError>> {
        todo!("Task 02: implement MysqlJobQueue::purge_completed")
    }

    fn replay_dead_letter(&self, _job_id: &str) -> BoxFuture<'_, Result<(), JobError>> {
        todo!("Task 02: implement MysqlJobQueue::replay_dead_letter")
    }
}
