//! Leader-elected distributed cron scheduler.

mod scheduler;
mod spec;

pub use scheduler::{BoxFuture, CronScheduler, CronSchedulerHandle};
pub use spec::{CronError, CronJobSpec, MissedTickPolicy, TickDecision};

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use chrono::{FixedOffset, TimeZone, Utc};
    use rume_data::coordination::lock::CoordinationFuture;
    use rume_data::coordination::{LeaderElector, LeaderLease};

    use super::*;

    #[derive(Debug, Default)]
    struct InMemoryLeaderElector {
        leases: Mutex<HashMap<String, LeaderLease>>,
    }

    impl InMemoryLeaderElector {
        fn new() -> Self {
            Self::default()
        }
    }

    impl LeaderElector for InMemoryLeaderElector {
        fn try_acquire<'a>(
            &'a self,
            role: &'a str,
            candidate_id: &'a str,
            ttl: Duration,
        ) -> CoordinationFuture<'a, Option<LeaderLease>> {
            Box::pin(async move {
                let mut leases = self.leases.lock().expect("lock");
                let now = Instant::now();
                let next_term = match leases.get(role) {
                    Some(existing) if !existing.is_expired() => {
                        if existing.candidate_id == candidate_id {
                            let updated = LeaderLease::new(role, candidate_id, existing.term)
                                .with_expires_at(now + ttl);
                            leases.insert(role.to_string(), updated.clone());
                            return Ok(Some(updated));
                        }
                        return Ok(None);
                    }
                    Some(existing) => existing.term + 1,
                    None => 1,
                };
                let lease =
                    LeaderLease::new(role, candidate_id, next_term).with_expires_at(now + ttl);
                leases.insert(role.to_string(), lease.clone());
                Ok(Some(lease))
            })
        }

        fn renew<'a>(
            &'a self,
            role: &'a str,
            candidate_id: &'a str,
            ttl: Duration,
        ) -> CoordinationFuture<'a, Option<LeaderLease>> {
            Box::pin(async move {
                let mut leases = self.leases.lock().expect("lock");
                let now = Instant::now();
                if let Some(existing) = leases.get_mut(role)
                    && !existing.is_expired()
                    && existing.candidate_id == candidate_id
                {
                    existing.expires_at = Some(now + ttl);
                    return Ok(Some(existing.clone()));
                }
                Ok(None)
            })
        }

        fn get_leader<'a>(&'a self, role: &'a str) -> CoordinationFuture<'a, Option<LeaderLease>> {
            Box::pin(async move {
                let leases = self.leases.lock().expect("lock");
                Ok(leases.get(role).filter(|l| !l.is_expired()).cloned())
            })
        }

        fn resign<'a>(
            &'a self,
            role: &'a str,
            candidate_id: &'a str,
        ) -> CoordinationFuture<'a, bool> {
            Box::pin(async move {
                let mut leases = self.leases.lock().expect("lock");
                if let Some(existing) = leases.get(role)
                    && existing.candidate_id == candidate_id
                {
                    leases.remove(role);
                    return Ok(true);
                }
                Ok(false)
            })
        }
    }

    #[test]
    fn cron_spec_rejects_invalid_expression_and_computes_next_tick_in_timezone() {
        assert!(CronJobSpec::new("bad", "not a cron").is_err());

        // 5-field expression: every day at 09:30 in UTC+05:30
        let ist = FixedOffset::east_opt(5 * 3600 + 1800).expect("valid offset");
        let spec_5 = CronJobSpec::new("daily-ist", "30 9 * * *")
            .expect("valid 5-field")
            .with_timezone(ist);
        let now = Utc.with_ymd_and_hms(2026, 6, 1, 3, 0, 0).unwrap();
        let next = spec_5.next_tick(now).expect("next tick");
        // 09:30 IST == 04:00 UTC
        assert_eq!(next, Utc.with_ymd_and_hms(2026, 6, 1, 4, 0, 0).unwrap());

        // 6-field expression: at second 15 of every minute
        let spec_6 = CronJobSpec::new("every-min-15s", "15 * * * * *").expect("valid 6-field");
        let now_6 = Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 10).unwrap();
        let next_6 = spec_6.next_tick(now_6).expect("next 6-field tick");
        assert_eq!(next_6, Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 15).unwrap());
    }

    #[tokio::test]
    async fn cron_scheduler_executes_only_on_elected_leader() {
        let elector = Arc::new(InMemoryLeaderElector::new());
        let count_a = Arc::new(AtomicUsize::new(0));
        let count_b = Arc::new(AtomicUsize::new(0));

        let mut sched_a = CronScheduler::new(
            elector.clone(),
            "cron-role",
            "node-a",
            Duration::from_millis(200),
        );
        let ca = Arc::clone(&count_a);
        sched_a
            .register(
                CronJobSpec::new("tick", "* * * * * *").expect("valid"),
                move || {
                    let ca = Arc::clone(&ca);
                    async move {
                        ca.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    }
                },
            )
            .expect("register a");

        let mut sched_b = CronScheduler::new(
            elector.clone(),
            "cron-role",
            "node-b",
            Duration::from_millis(200),
        );
        let cb = Arc::clone(&count_b);
        sched_b
            .register(
                CronJobSpec::new("tick", "* * * * * *").expect("valid"),
                move || {
                    let cb = Arc::clone(&cb);
                    async move {
                        cb.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    }
                },
            )
            .expect("register b");

        let handle_a = sched_a.start();
        let handle_b = sched_b.start();

        tokio::time::sleep(Duration::from_millis(2200)).await;

        let a = count_a.load(Ordering::SeqCst);
        let b = count_b.load(Ordering::SeqCst);
        assert!(a + b >= 1, "expected at least one tick, got a={a}, b={b}");
        assert!(
            a == 0 || b == 0,
            "only one leader should execute ticks, got a={a}, b={b}"
        );

        handle_a
            .shutdown(Duration::from_secs(1))
            .await
            .expect("shutdown a");
        handle_b
            .shutdown(Duration::from_secs(1))
            .await
            .expect("shutdown b");
    }

    #[tokio::test]
    async fn cron_scheduler_fails_over_when_leader_steps_down() {
        let elector = Arc::new(InMemoryLeaderElector::new());
        let count_a = Arc::new(AtomicUsize::new(0));
        let count_b = Arc::new(AtomicUsize::new(0));

        let mut sched_a = CronScheduler::new(
            elector.clone(),
            "cron-failover",
            "node-a",
            Duration::from_millis(200),
        );
        let ca = Arc::clone(&count_a);
        sched_a
            .register(
                CronJobSpec::new("tick", "* * * * * *").expect("valid"),
                move || {
                    let ca = Arc::clone(&ca);
                    async move {
                        ca.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    }
                },
            )
            .expect("register a");

        let handle_a = sched_a.start();
        tokio::time::sleep(Duration::from_millis(1200)).await;
        assert!(count_a.load(Ordering::SeqCst) >= 1, "primary should tick");

        let mut sched_b = CronScheduler::new(
            elector.clone(),
            "cron-failover",
            "node-b",
            Duration::from_millis(200),
        );
        let cb = Arc::clone(&count_b);
        sched_b
            .register(
                CronJobSpec::new("tick", "* * * * * *").expect("valid"),
                move || {
                    let cb = Arc::clone(&cb);
                    async move {
                        cb.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    }
                },
            )
            .expect("register b");

        let handle_b = sched_b.start();
        handle_a
            .shutdown(Duration::from_secs(1))
            .await
            .expect("shutdown a");

        tokio::time::sleep(Duration::from_millis(1300)).await;
        assert!(
            count_b.load(Ordering::SeqCst) >= 1,
            "standby should take over and tick after primary resigns"
        );

        handle_b
            .shutdown(Duration::from_secs(1))
            .await
            .expect("shutdown b");
    }

    #[test]
    fn cron_scheduler_respects_missed_tick_policy() {
        let scheduled = Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 6, 1, 12, 3, 10).unwrap();

        let skip_spec = CronJobSpec::new("skip-job", "0 * * * * *")
            .expect("valid")
            .with_missed_tick_policy(MissedTickPolicy::Skip);
        let skip_decision = skip_spec.evaluate_tick(scheduled, now).expect("eval skip");
        assert!(!skip_decision.should_run);
        assert_eq!(
            skip_decision.next_scheduled,
            Utc.with_ymd_and_hms(2026, 6, 1, 12, 4, 0).unwrap()
        );

        let run_once_spec = CronJobSpec::new("run-once-job", "0 * * * * *")
            .expect("valid")
            .with_missed_tick_policy(MissedTickPolicy::RunOnce);
        let run_once_decision = run_once_spec
            .evaluate_tick(scheduled, now)
            .expect("eval run_once");
        assert!(run_once_decision.should_run);
        assert_eq!(
            run_once_decision.next_scheduled,
            Utc.with_ymd_and_hms(2026, 6, 1, 12, 4, 0).unwrap()
        );
    }

    #[tokio::test]
    async fn cron_scheduler_stops_firing_when_renew_stalls_past_lease_ttl() {
        use std::sync::atomic::AtomicBool;

        #[derive(Debug)]
        struct StallingRenewElector {
            acquired: AtomicBool,
        }

        impl LeaderElector for StallingRenewElector {
            fn try_acquire<'a>(
                &'a self,
                role: &'a str,
                candidate_id: &'a str,
                ttl: Duration,
            ) -> CoordinationFuture<'a, Option<LeaderLease>> {
                Box::pin(async move {
                    if !self.acquired.swap(true, Ordering::SeqCst) {
                        Ok(Some(
                            LeaderLease::new(role, candidate_id, 1)
                                .with_expires_at(Instant::now() + ttl),
                        ))
                    } else {
                        // Once leadership is lost after stalled renew, refuse re-acquisition.
                        Ok(None)
                    }
                })
            }

            fn renew<'a>(
                &'a self,
                _role: &'a str,
                _candidate_id: &'a str,
                _ttl: Duration,
            ) -> CoordinationFuture<'a, Option<LeaderLease>> {
                Box::pin(async move {
                    // Stall far longer than lease_ttl (150ms).
                    tokio::time::sleep(Duration::from_secs(10)).await;
                    Ok(None)
                })
            }

            fn get_leader<'a>(
                &'a self,
                _role: &'a str,
            ) -> CoordinationFuture<'a, Option<LeaderLease>> {
                Box::pin(async move { Ok(None) })
            }

            fn resign<'a>(
                &'a self,
                _role: &'a str,
                _candidate_id: &'a str,
            ) -> CoordinationFuture<'a, bool> {
                Box::pin(async move { Ok(true) })
            }
        }

        let elector = Arc::new(StallingRenewElector {
            acquired: AtomicBool::new(false),
        });
        let fires = Arc::new(AtomicUsize::new(0));
        let fires_clone = Arc::clone(&fires);

        // Lease TTL is 150ms, far shorter than the 1-second cron tick interval, so by the
        // time the first tick would arrive, the stalled renew has timed out and stepped down.
        let mut sched = CronScheduler::new(
            elector,
            "cron-stalled-renew",
            "node-stalled",
            Duration::from_millis(150),
        );
        sched
            .register(
                CronJobSpec::new("tick", "* * * * * *").expect("valid"),
                move || {
                    let fires_clone = Arc::clone(&fires_clone);
                    async move {
                        fires_clone.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    }
                },
            )
            .expect("register");

        let handle = sched.start();
        // Wait > 1.3s: if renew wasn't bounded / deadline-fenced, the 1s tick would fire.
        tokio::time::sleep(Duration::from_millis(1350)).await;

        assert_eq!(
            fires.load(Ordering::SeqCst),
            0,
            "scheduler with stalled renew() past lease_ttl must NOT fire ticks"
        );

        handle
            .shutdown(Duration::from_secs(1))
            .await
            .expect("shutdown");
    }

    #[tokio::test]
    async fn cron_standby_takeover_skips_stale_ticks_and_attaches_deterministic_idempotency_key() {
        use std::sync::atomic::AtomicBool;

        use serde::{Deserialize, Serialize};

        use crate::jobs::{Job, JobError, JobQueue, QueuedJob};

        #[derive(Debug, Serialize, Deserialize)]
        struct CronPingJob;

        impl Job for CronPingJob {
            const NAME: &'static str = "cron.ping";
        }

        #[derive(Debug, Default)]
        struct CapturingQueue {
            keys: Mutex<Vec<(Option<String>, Instant)>>,
            enqueued: tokio::sync::Notify,
        }

        impl JobQueue for CapturingQueue {
            fn enqueue_delayed_with_trace<J: Job>(
                &self,
                job: J,
                _delay: Duration,
                _trace_context: Option<String>,
            ) -> crate::jobs::BoxFuture<'_, Result<String, JobError>> {
                self.keys
                    .lock()
                    .unwrap()
                    .push((job.idempotency_key(), Instant::now()));
                self.enqueued.notify_one();
                Box::pin(async { Ok("job-1".to_string()) })
            }

            fn dequeue_for(
                &self,
                _worker_id: &str,
            ) -> crate::jobs::BoxFuture<'_, Result<Option<QueuedJob>, JobError>> {
                Box::pin(async { Ok(None) })
            }

            fn reclaim_abandoned_jobs(
                &self,
                _dead_workers: &[String],
            ) -> crate::jobs::BoxFuture<'_, Result<usize, JobError>> {
                Box::pin(async { Ok(0) })
            }

            fn ack(
                &self,
                _job_id: &str,
                _lease_token: &str,
            ) -> crate::jobs::BoxFuture<'_, Result<(), JobError>> {
                Box::pin(async { Ok(()) })
            }

            fn nack(
                &self,
                _job: QueuedJob,
                _lease_token: &str,
                _delay: Duration,
            ) -> crate::jobs::BoxFuture<'_, Result<(), JobError>> {
                Box::pin(async { Ok(()) })
            }

            fn dead_letter(
                &self,
                _job: QueuedJob,
                _lease_token: &str,
                _reason: String,
            ) -> crate::jobs::BoxFuture<'_, Result<(), JobError>> {
                Box::pin(async { Ok(()) })
            }

            fn pending_count(&self) -> crate::jobs::BoxFuture<'_, Result<usize, JobError>> {
                Box::pin(async { Ok(0) })
            }

            fn dead_letter_count(&self) -> crate::jobs::BoxFuture<'_, Result<usize, JobError>> {
                Box::pin(async { Ok(0) })
            }
        }

        #[derive(Debug)]
        struct GatedElector {
            allow_leader: AtomicBool,
        }

        impl LeaderElector for GatedElector {
            fn try_acquire<'a>(
                &'a self,
                role: &'a str,
                candidate_id: &'a str,
                ttl: Duration,
            ) -> CoordinationFuture<'a, Option<LeaderLease>> {
                Box::pin(async move {
                    if self.allow_leader.load(Ordering::SeqCst) {
                        Ok(Some(
                            LeaderLease::new(role, candidate_id, 1)
                                .with_expires_at(Instant::now() + ttl),
                        ))
                    } else {
                        Ok(None)
                    }
                })
            }

            fn renew<'a>(
                &'a self,
                role: &'a str,
                candidate_id: &'a str,
                ttl: Duration,
            ) -> CoordinationFuture<'a, Option<LeaderLease>> {
                Box::pin(async move {
                    if self.allow_leader.load(Ordering::SeqCst) {
                        Ok(Some(
                            LeaderLease::new(role, candidate_id, 1)
                                .with_expires_at(Instant::now() + ttl),
                        ))
                    } else {
                        Ok(None)
                    }
                })
            }

            fn get_leader<'a>(
                &'a self,
                _role: &'a str,
            ) -> CoordinationFuture<'a, Option<LeaderLease>> {
                Box::pin(async move { Ok(None) })
            }

            fn resign<'a>(
                &'a self,
                _role: &'a str,
                _candidate_id: &'a str,
            ) -> CoordinationFuture<'a, bool> {
                Box::pin(async move { Ok(true) })
            }
        }

        let elector = Arc::new(GatedElector {
            allow_leader: AtomicBool::new(false),
        });
        let queue = Arc::new(CapturingQueue::default());

        let mut sched = CronScheduler::new(
            elector.clone(),
            "cron-standby",
            "node-standby",
            Duration::from_millis(200),
        );
        // Use RunOnce so that if standby didn't recompute `next_scheduled` on takeover,
        // it would immediately fire a bogus catch-up tick upon acquiring leadership.
        let spec = CronJobSpec::new("heartbeat", "* * * * * *")
            .expect("valid")
            .with_missed_tick_policy(MissedTickPolicy::RunOnce);
        sched
            .register_job(spec, Arc::clone(&queue), || CronPingJob)
            .expect("register_job");

        let handle = sched.start();

        // Remain in standby across at least 2 full 1-second ticks so the initial
        // `next_scheduled` is firmly in the past.
        tokio::time::sleep(Duration::from_millis(2100)).await;
        assert!(
            queue.keys.lock().unwrap().is_empty(),
            "standby must not enqueue jobs before becoming leader"
        );

        let takeover_unix_ts = Utc::now().timestamp();
        let first_tick = queue.enqueued.notified();
        elector.allow_leader.store(true, Ordering::SeqCst);

        // Wait deterministically for the first enqueued tick after takeover, then shut down
        // immediately so wall-clock alignment or runner jitter cannot cross into a second tick.
        tokio::time::timeout(Duration::from_secs(3), first_tick)
            .await
            .expect("standby takeover must fire a future tick within 3s");

        handle
            .shutdown(Duration::from_secs(1))
            .await
            .expect("shutdown");

        let recorded = queue.keys.lock().unwrap().clone();
        assert!(
            !recorded.is_empty(),
            "standby takeover must fire at least one future tick"
        );
        for (raw_key, _) in &recorded {
            let key = raw_key
                .as_deref()
                .expect("enqueued cron job must carry deterministic idempotency key");
            assert!(
                key.starts_with("cron:heartbeat:"),
                "expected key prefix 'cron:heartbeat:', got '{key}'"
            );
            let key_ts: i64 = key
                .strip_prefix("cron:heartbeat:")
                .unwrap()
                .parse()
                .expect("unix timestamp suffix");
            assert!(
                key_ts > takeover_unix_ts,
                "scheduled tick timestamp {key_ts} must be strictly after takeover {takeover_unix_ts} (not a stale pre-takeover tick)"
            );
        }
    }

    #[tokio::test]
    async fn cron_scheduler_isolates_handler_panics_and_continues_next_tick() {
        let elector = Arc::new(InMemoryLeaderElector::new());
        let attempts = Arc::new(AtomicUsize::new(0));
        let completed = Arc::new(AtomicUsize::new(0));

        let mut sched = CronScheduler::new(
            elector,
            "cron-panic-isolation",
            "node-1",
            Duration::from_secs(5),
        );
        let spec = CronJobSpec::new("panicky", "* * * * * *").expect("valid 6-field cron");
        let attempts_cb = Arc::clone(&attempts);
        let completed_cb = Arc::clone(&completed);
        sched
            .register(spec, move || {
                let n = attempts_cb.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    // Synchronous panic during future construction on tick 0.
                    panic!("sync panic on tick 0");
                }
                let completed_inner = Arc::clone(&completed_cb);
                async move {
                    if n == 1 {
                        // Asynchronous panic during future polling on tick 1.
                        panic!("async panic on tick 1");
                    }
                    completed_inner.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            })
            .expect("register");

        let handle = sched.start();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while completed.load(Ordering::SeqCst) == 0 && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        handle
            .shutdown(Duration::from_secs(1))
            .await
            .expect("shutdown");

        assert!(
            attempts.load(Ordering::SeqCst) >= 3,
            "scheduler must survive both sync and async panics and fire subsequent ticks"
        );
        assert!(
            completed.load(Ordering::SeqCst) >= 1,
            "tick after panics must execute cleanly"
        );
    }
}
