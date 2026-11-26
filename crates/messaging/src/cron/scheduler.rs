//! Leader-elected distributed cron scheduler implementation.

use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use rume_data::coordination::LeaderElector;
use tokio::sync::watch;
use tokio::task::JoinSet;
use tracing::{debug, error, warn};

use crate::cron::spec::{CronError, CronJobSpec};
use crate::jobs::{Job, JobQueue};

/// Pinned boxed future type alias for cron job callbacks.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

type CronHandler = Arc<dyn Fn() -> BoxFuture<'static, Result<(), CronError>> + Send + Sync>;

struct RegisteredCronJob {
    spec: CronJobSpec,
    handler: CronHandler,
}

/// Leader-elected cron scheduler ensuring each recurring job runs once per cluster.
pub struct CronScheduler {
    elector: Arc<dyn LeaderElector>,
    role: String,
    candidate_id: String,
    lease_ttl: Duration,
    jobs: Vec<RegisteredCronJob>,
}

impl Debug for CronScheduler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CronScheduler")
            .field("role", &self.role)
            .field("candidate_id", &self.candidate_id)
            .field("lease_ttl", &self.lease_ttl)
            .field("job_count", &self.jobs.len())
            .finish_non_exhaustive()
    }
}

impl CronScheduler {
    /// Creates a new leader-elected cron scheduler.
    pub fn new(
        elector: Arc<dyn LeaderElector>,
        role: impl Into<String>,
        candidate_id: impl Into<String>,
        lease_ttl: Duration,
    ) -> Self {
        Self {
            elector,
            role: role.into(),
            candidate_id: candidate_id.into(),
            lease_ttl,
            jobs: Vec::new(),
        }
    }

    /// Registers a recurring cron job with an async handler closure.
    pub fn register<F, Fut>(&mut self, spec: CronJobSpec, handler: F) -> Result<(), CronError>
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), CronError>> + Send + 'static,
    {
        if self.jobs.iter().any(|job| job.spec.name == spec.name) {
            return Err(CronError::Execution(format!(
                "duplicate cron job name '{}'",
                spec.name
            )));
        }

        let handler: CronHandler = Arc::new(move || Box::pin(handler()));
        self.jobs.push(RegisteredCronJob { spec, handler });
        Ok(())
    }

    /// Registers a recurring cron job that enqueues a [`Job`] onto a [`JobQueue`].
    pub fn register_job<Q, J, F>(
        &mut self,
        spec: CronJobSpec,
        queue: Arc<Q>,
        job_factory: F,
    ) -> Result<(), CronError>
    where
        Q: JobQueue + 'static,
        J: Job,
        F: Fn() -> J + Send + Sync + 'static,
    {
        let factory = Arc::new(job_factory);
        self.register(spec, move || {
            let queue = Arc::clone(&queue);
            let job = factory();
            async move {
                queue
                    .enqueue(job)
                    .await
                    .map(|_| ())
                    .map_err(|err| CronError::Execution(err.to_string()))
            }
        })
    }

    /// Starts the leader-election loop and cron scheduling tasks.
    pub fn start(self) -> CronSchedulerHandle {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (leader_tx, leader_rx) = watch::channel(false);
        let mut tasks = JoinSet::new();

        let elector = self.elector;
        let role = self.role;
        let candidate_id = self.candidate_id;
        let lease_ttl = self.lease_ttl;
        let mut election_shutdown = shutdown_rx.clone();

        tasks.spawn(async move {
            run_election_loop(
                elector,
                role,
                candidate_id,
                lease_ttl,
                leader_tx,
                &mut election_shutdown,
            )
            .await;
        });

        for job in self.jobs {
            let mut job_shutdown = shutdown_rx.clone();
            let mut job_leader = leader_rx.clone();
            tasks.spawn(async move {
                run_job_loop(job, &mut job_leader, &mut job_shutdown).await;
            });
        }

        CronSchedulerHandle { shutdown_tx, tasks }
    }
}

async fn run_election_loop(
    elector: Arc<dyn LeaderElector>,
    role: String,
    candidate_id: String,
    lease_ttl: Duration,
    leader_tx: watch::Sender<bool>,
    shutdown_rx: &mut watch::Receiver<bool>,
) {
    // Why: renew at 1/3 of lease TTL (minimum 10ms) so transient jitter does not expire
    // leadership before the next renewal, while short test TTLs still fail over quickly.
    let renew_interval = (lease_ttl / 3).max(Duration::from_millis(10));
    let mut is_leader = false;

    loop {
        if *shutdown_rx.borrow() {
            break;
        }

        if is_leader {
            match elector.renew(&role, &candidate_id, lease_ttl).await {
                Ok(Some(_)) => {}
                Ok(None) => {
                    warn!(role = %role, candidate_id = %candidate_id, "lost cron leadership lease");
                    is_leader = false;
                    let _ = leader_tx.send(false);
                }
                Err(err) => {
                    warn!(role = %role, candidate_id = %candidate_id, error = %err, "cron leadership renewal failed");
                    is_leader = false;
                    let _ = leader_tx.send(false);
                }
            }
        } else {
            match elector.try_acquire(&role, &candidate_id, lease_ttl).await {
                Ok(Some(_)) => {
                    debug!(role = %role, candidate_id = %candidate_id, "acquired cron leadership");
                    is_leader = true;
                    let _ = leader_tx.send(true);
                }
                Ok(None) => {}
                Err(err) => {
                    warn!(role = %role, candidate_id = %candidate_id, error = %err, "cron leadership acquisition failed");
                }
            }
        }

        tokio::select! {
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    break;
                }
            }
            _ = tokio::time::sleep(renew_interval) => {}
        }
    }

    if is_leader && let Err(err) = elector.resign(&role, &candidate_id).await {
        warn!(role = %role, candidate_id = %candidate_id, error = %err, "failed to resign cron leadership on shutdown");
    }
}

async fn run_job_loop(
    job: RegisteredCronJob,
    leader_rx: &mut watch::Receiver<bool>,
    shutdown_rx: &mut watch::Receiver<bool>,
) {
    let mut next_scheduled = match job.spec.next_tick(Utc::now()) {
        Ok(next) => next,
        Err(err) => {
            error!(job = %job.spec.name, error = %err, "failed to compute initial cron tick");
            return;
        }
    };

    loop {
        if *shutdown_rx.borrow() {
            break;
        }

        if !*leader_rx.borrow() {
            tokio::select! {
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        break;
                    }
                }
                res = leader_rx.changed() => {
                    if res.is_err() {
                        break;
                    }
                }
            }
            continue;
        }

        let now = Utc::now();
        if now < next_scheduled {
            let wait = (next_scheduled - now).to_std().unwrap_or(Duration::ZERO);
            tokio::select! {
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        break;
                    }
                }
                res = leader_rx.changed() => {
                    if res.is_err() {
                        break;
                    }
                    continue;
                }
                _ = tokio::time::sleep(wait) => {}
            }
        }

        if *shutdown_rx.borrow() || !*leader_rx.borrow() {
            continue;
        }

        let eval_now = Utc::now();
        match job.spec.evaluate_tick(next_scheduled, eval_now) {
            Ok(decision) => {
                next_scheduled = decision.next_scheduled;
                if decision.should_run
                    && *leader_rx.borrow()
                    && !*shutdown_rx.borrow()
                    && let Err(err) = (job.handler)().await
                {
                    error!(job = %job.spec.name, error = %err, "cron job handler failed");
                }
            }
            Err(err) => {
                error!(job = %job.spec.name, error = %err, "failed to evaluate cron tick");
                break;
            }
        }
    }
}

/// Handle for gracefully shutting down a running [`CronScheduler`].
#[derive(Debug)]
pub struct CronSchedulerHandle {
    shutdown_tx: watch::Sender<bool>,
    tasks: JoinSet<()>,
}

impl CronSchedulerHandle {
    /// Signals shutdown, resigns leadership, and awaits running tasks up to `timeout`.
    pub async fn shutdown(mut self, timeout: Duration) -> Result<(), CronError> {
        let _ = self.shutdown_tx.send(true);
        let drain = async {
            while let Some(res) = self.tasks.join_next().await {
                if let Err(err) = res
                    && !err.is_cancelled()
                {
                    warn!(error = %err, "cron background task panicked during shutdown");
                }
            }
        };

        if tokio::time::timeout(timeout, drain).await.is_err() {
            self.tasks.abort_all();
            return Err(CronError::Execution(
                "cron scheduler shutdown timed out".to_string(),
            ));
        }

        Ok(())
    }
}
