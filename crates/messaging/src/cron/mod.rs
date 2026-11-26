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
}
