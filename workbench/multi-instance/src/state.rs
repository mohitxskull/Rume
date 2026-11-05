use rume::jobs::JobWorker;
use rume::jobs::postgres::PostgresJobQueue;
use rume::outbox::OutboxRelay;
use rume::persistence::pool::DatabasePool;
use rume::persistence::postgres::PostgresBackend;
use std::sync::Arc;

use crate::jobs::NotificationHandler;

/// Shared application state for Rume Workbench.
#[derive(Clone)]
pub struct WorkbenchState {
    pub pool: DatabasePool<PostgresBackend>,
    pub job_queue: Arc<PostgresJobQueue>,
    pub notification_handler: NotificationHandler,
    pub relay: Option<OutboxRelay<DatabasePool<PostgresBackend>>>,
    pub worker: Option<JobWorker<PostgresJobQueue>>,
}
