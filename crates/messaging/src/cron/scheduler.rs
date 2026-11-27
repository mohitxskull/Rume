//! Leader-elected distributed cron scheduler implementation.

use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use rume_data::coordination::LeaderElector;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio::time::Instant;
use tracing::{debug, error, warn};

use crate::cron::spec::{CronError, CronJobSpec};
use crate::jobs::{Job, JobQueue, RetryPolicy};

/// Pinned boxed future type alias for cron job callbacks.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

type CronHandler =
    Arc<dyn Fn(DateTime<Utc>) -> BoxFuture<'static, Result<(), CronError>> + Send + Sync>;

/// Wrapper attaching a deterministic `cron:{job_name}:{unix_ts}` idempotency key to a typed
/// [`Job`] without altering its serialized payload or metadata.
struct CronIdempotentJob<J> {
    inner: J,
    idempotency_key: String,
}

impl<J: Serialize> Serialize for CronIdempotentJob<J> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.inner.serialize(serializer)
    }
}

impl<'de, J: Deserialize<'de>> Deserialize<'de> for CronIdempotentJob<J> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        J::deserialize(deserializer).map(|inner| Self {
            inner,
            idempotency_key: String::new(),
        })
    }
}

impl<J: Job> Job for CronIdempotentJob<J> {
    const NAME: &'static str = J::NAME;

    fn retry_policy(&self) -> RetryPolicy {
        self.inner.retry_policy()
    }

    fn timeout(&self) -> Option<Duration> {
        self.inner.timeout()
    }

    fn idempotency_key(&self) -> Option<String> {
        Some(self.idempotency_key.clone())
    }

    fn tenant_id(&self) -> Option<String> {
        self.inner.tenant_id()
    }

    fn trace_context(&self) -> Option<String> {
        self.inner.trace_context()
    }

    fn queue(&self) -> &str {
        self.inner.queue()
    }

    fn priority(&self) -> i32 {
        self.inner.priority()
    }
}

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
        self.register_with_scheduled(spec, move |_scheduled_at| handler())
    }

    fn register_with_scheduled<F, Fut>(
        &mut self,
        spec: CronJobSpec,
        handler: F,
    ) -> Result<(), CronError>
    where
        F: Fn(DateTime<Utc>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), CronError>> + Send + 'static,
    {
        if self.jobs.iter().any(|job| job.spec.name == spec.name) {
            return Err(CronError::Execution(format!(
                "duplicate cron job name '{}'",
                spec.name
            )));
        }

        let handler: CronHandler = Arc::new(move |scheduled_at| Box::pin(handler(scheduled_at)));
        self.jobs.push(RegisteredCronJob { spec, handler });
        Ok(())
    }

    /// Registers a recurring cron job that enqueues a [`Job`] onto a [`JobQueue`] with a
    /// deterministic `cron:{job_name}:{unix_ts}` idempotency key so failover within the same
    /// tick cannot double-execute.
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
        let job_name = spec.name.clone();
        let factory = Arc::new(job_factory);
        self.register_with_scheduled(spec, move |scheduled_at| {
            let queue = Arc::clone(&queue);
            let idempotency_key = format!("cron:{job_name}:{}", scheduled_at.timestamp());
            let wrapped = CronIdempotentJob {
                inner: factory(),
                idempotency_key,
            };
            async move {
                queue
                    .enqueue(wrapped)
                    .await
                    .map(|_| ())
                    .map_err(|err| CronError::Execution(err.to_string()))
            }
        })
    }

    /// Starts the leader-election loop and cron scheduling tasks.
    pub fn start(self) -> CronSchedulerHandle {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (leader_tx, leader_rx) = watch::channel::<Option<Instant>>(None);
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

fn is_leader_valid(leader_rx: &watch::Receiver<Option<Instant>>) -> bool {
    leader_rx
        .borrow()
        .is_some_and(|deadline| Instant::now() < deadline)
}

async fn run_election_loop(
    elector: Arc<dyn LeaderElector>,
    role: String,
    candidate_id: String,
    lease_ttl: Duration,
    leader_tx: watch::Sender<Option<Instant>>,
    shutdown_rx: &mut watch::Receiver<bool>,
) {
    // Why: renew at 1/3 of lease TTL (minimum 10ms) so transient jitter does not expire
    // leadership before the next renewal, while short test TTLs still fail over quickly.
    let renew_interval = (lease_ttl / 3).max(Duration::from_millis(10));
    let mut leader_deadline: Option<Instant> = None;

    loop {
        if *shutdown_rx.borrow() {
            break;
        }

        if let Some(deadline) = leader_deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                warn!(role = %role, candidate_id = %candidate_id, "cron leadership lease expired locally");
                leader_deadline = None;
                let _ = leader_tx.send(None);
                continue;
            }
            // Why: bound `elector.renew` by the remaining local lease TTL so a hung
            // coordinator/network call cannot keep `leader_tx` armed past lease expiry
            // and cause split-brain double-firing. Measure `call_start` before the RPC so
            // network latency never inflates our local belief beyond server expiry.
            let call_start = Instant::now();
            let renew_res = tokio::select! {
                biased;
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        break;
                    }
                    continue;
                }
                res = tokio::time::timeout(remaining, elector.renew(&role, &candidate_id, lease_ttl)) => res,
            };
            match renew_res {
                Ok(Ok(Some(_))) => {
                    let next_deadline = call_start + lease_ttl;
                    leader_deadline = Some(next_deadline);
                    let _ = leader_tx.send(leader_deadline);
                }
                Ok(Ok(None)) => {
                    warn!(role = %role, candidate_id = %candidate_id, "lost cron leadership lease");
                    leader_deadline = None;
                    let _ = leader_tx.send(None);
                }
                Ok(Err(err)) => {
                    warn!(role = %role, candidate_id = %candidate_id, error = %err, "cron leadership renewal failed");
                    leader_deadline = None;
                    let _ = leader_tx.send(None);
                }
                Err(_) => {
                    warn!(role = %role, candidate_id = %candidate_id, "cron leadership renewal timed out; stepping down");
                    leader_deadline = None;
                    let _ = leader_tx.send(None);
                }
            }
        } else {
            let call_start = Instant::now();
            let acquire_res = tokio::select! {
                biased;
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        break;
                    }
                    continue;
                }
                res = tokio::time::timeout(lease_ttl, elector.try_acquire(&role, &candidate_id, lease_ttl)) => res,
            };
            match acquire_res {
                Ok(Ok(Some(_))) => {
                    debug!(role = %role, candidate_id = %candidate_id, "acquired cron leadership");
                    let next_deadline = call_start + lease_ttl;
                    leader_deadline = Some(next_deadline);
                    let _ = leader_tx.send(leader_deadline);
                }
                Ok(Ok(None)) => {}
                Ok(Err(err)) => {
                    warn!(role = %role, candidate_id = %candidate_id, error = %err, "cron leadership acquisition failed");
                }
                Err(_) => {
                    warn!(role = %role, candidate_id = %candidate_id, "cron leadership acquisition timed out");
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

    let _ = leader_tx.send(None);
    if leader_deadline.is_some()
        && let Ok(Err(err)) =
            tokio::time::timeout(lease_ttl, elector.resign(&role, &candidate_id)).await
    {
        warn!(role = %role, candidate_id = %candidate_id, error = %err, "failed to resign cron leadership on shutdown");
    }
}

async fn run_job_loop(
    job: RegisteredCronJob,
    leader_rx: &mut watch::Receiver<Option<Instant>>,
    shutdown_rx: &mut watch::Receiver<bool>,
) {
    let mut next_scheduled = match job.spec.next_tick(Utc::now()) {
        Ok(next) => next,
        Err(err) => {
            error!(job = %job.spec.name, error = %err, "failed to compute initial cron tick");
            return;
        }
    };
    let mut was_leader = is_leader_valid(leader_rx);

    loop {
        if *shutdown_rx.borrow() {
            break;
        }

        if !is_leader_valid(leader_rx) {
            was_leader = false;
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

        // Why: on non-leader -> leader transition, recompute `next_scheduled` from `now`
        // so a long-running standby never fires stale missed ticks from before it became
        // leader (which the previous leader already executed).
        if !was_leader {
            match job.spec.next_tick(Utc::now()) {
                Ok(next) => next_scheduled = next,
                Err(err) => {
                    error!(job = %job.spec.name, error = %err, "failed to recompute cron tick on leadership acquisition");
                    break;
                }
            }
            was_leader = true;
        }

        let now = Utc::now();
        if now < next_scheduled {
            let wait = (next_scheduled - now).to_std().unwrap_or(Duration::ZERO);
            let remaining_lease = leader_rx
                .borrow()
                .map(|d| d.saturating_duration_since(Instant::now()))
                .unwrap_or(Duration::ZERO);
            if remaining_lease.is_zero() {
                was_leader = false;
                continue;
            }
            let sleep_dur = wait.min(remaining_lease);
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
                _ = tokio::time::sleep(sleep_dur) => {}
            }
        }

        if *shutdown_rx.borrow() || !is_leader_valid(leader_rx) {
            if !is_leader_valid(leader_rx) {
                was_leader = false;
            }
            continue;
        }

        let eval_now = Utc::now();
        if eval_now < next_scheduled {
            continue;
        }
        let scheduled_at = next_scheduled;
        match job.spec.evaluate_tick(scheduled_at, eval_now) {
            Ok(decision) => {
                next_scheduled = decision.next_scheduled;
                if decision.should_run
                    && is_leader_valid(leader_rx)
                    && !*shutdown_rx.borrow()
                    && let Err(err) = (job.handler)(scheduled_at).await
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
