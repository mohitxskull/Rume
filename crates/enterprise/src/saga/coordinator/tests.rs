use super::*;
use crate::saga::error::SagaError;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

type FailPred = Arc<dyn Fn(&SagaRecord) -> bool + Send + Sync>;

#[derive(Clone, Default)]
struct FaultySagaStore {
    inner: InMemorySagaStore,
    fail_on: Arc<Mutex<Option<FailPred>>>,
}

impl FaultySagaStore {
    fn new() -> Self {
        Self::default()
    }

    fn fail_when<F>(&self, pred: F)
    where
        F: Fn(&SagaRecord) -> bool + Send + Sync + 'static,
    {
        *self.fail_on.lock().expect("lock") = Some(Arc::new(pred));
    }

    fn clear_failure(&self) {
        *self.fail_on.lock().expect("lock") = None;
    }
}

impl SagaStore for FaultySagaStore {
    fn save_record_cas(
        &self,
        record: &SagaRecord,
        expected_version: u64,
    ) -> BoxFuture<'_, Result<(), SagaError>> {
        let should_fail = self
            .fail_on
            .lock()
            .expect("lock")
            .as_ref()
            .is_some_and(|pred| pred(record));
        if should_fail {
            return Box::pin(async {
                Err(SagaError::Storage {
                    saga_id: None,
                    message: "injected storage failure".into(),
                })
            });
        }
        self.inner.save_record_cas(record, expected_version)
    }

    fn load_record(&self, saga_id: &str) -> BoxFuture<'_, Result<Option<SagaRecord>, SagaError>> {
        self.inner.load_record(saga_id)
    }

    fn list_incomplete(&self) -> BoxFuture<'_, Result<Vec<SagaRecord>, SagaError>> {
        self.inner.list_incomplete()
    }

    fn try_acquire_execution(&self, saga_id: &str) -> BoxFuture<'_, Result<String, SagaError>> {
        self.inner.try_acquire_execution(saga_id)
    }

    fn release_execution(
        &self,
        saga_id: &str,
        claim_token: &str,
    ) -> BoxFuture<'_, Result<(), SagaError>> {
        self.inner.release_execution(saga_id, claim_token)
    }

    fn release_execution_sync(&self, saga_id: &str, claim_token: &str) -> bool {
        self.inner.release_execution_sync(saga_id, claim_token)
    }
}

#[tokio::test]
async fn run_includes_generated_saga_id_in_storage_error() {
    let store = Arc::new(FaultySagaStore::new());
    store.fail_when(
        |r| matches!(&r.status, SagaStatus::Running { current_step } if current_step == "step_2"),
    );

    let saga = Saga::new("id_in_storage_err")
        .with_store(store.clone())
        .step("step_1", |_| async { Ok(()) }, |_| async { Ok(()) })
        .step("step_2", |_| async { Ok(()) }, |_| async { Ok(()) });

    let err1 = saga
        .run(())
        .await
        .err()
        .expect("first run should fail on step_2 Running write");
    let err2 = saga
        .run(())
        .await
        .err()
        .expect("second run should fail on step_2 Running write");

    let id1 = err1
        .saga_id()
        .expect("storage error must include generated saga_id")
        .to_string();
    let id2 = err2
        .saga_id()
        .expect("storage error must include generated saga_id")
        .to_string();
    assert_ne!(id1, id2, "each run must report its own distinct saga_id");

    store.clear_failure();
    let report1 = saga.resume(&id1, ()).await.expect("resume 1");
    let report2 = saga.resume(&id2, ()).await.expect("resume 2");
    assert!(report1.status.is_success());
    assert!(report2.status.is_success());
}

#[tokio::test]
async fn run_recoverable_preserves_mutated_context_across_storage_failure() {
    #[derive(Default)]
    struct Ctx {
        reservation_id: Option<String>,
        observed_by_b: Option<String>,
    }

    let store = Arc::new(FaultySagaStore::new());
    store.fail_when(
        |r| matches!(&r.status, SagaStatus::Running { current_step } if current_step == "step_b"),
    );

    let a_runs = Arc::new(AtomicUsize::new(0));
    let a_runs_c = Arc::clone(&a_runs);

    let saga = Saga::new("preserve_ctx")
        .with_store(store.clone())
        .step(
            "step_a",
            move |ctx: &mut Ctx| {
                a_runs_c.fetch_add(1, Ordering::SeqCst);
                ctx.reservation_id = Some("resv-42".into());
                async { Ok(()) }
            },
            |_| async { Ok(()) },
        )
        .step(
            "step_b",
            |ctx: &mut Ctx| {
                ctx.observed_by_b = ctx.reservation_id.clone();
                async { Ok(()) }
            },
            |_| async { Ok(()) },
        );

    let err = saga
        .run_recoverable(Ctx::default())
        .await
        .err()
        .expect("step_b Running write should fail");
    assert_eq!(
        err.handle.context.reservation_id.as_deref(),
        Some("resv-42")
    );
    assert_eq!(err.handle.completed_steps, vec!["step_a"]);

    store.clear_failure();
    let report = saga
        .resume_from_handle(*err.handle)
        .await
        .expect("resume_from_handle should succeed");
    assert!(report.status.is_success());
    assert_eq!(a_runs.load(Ordering::SeqCst), 1, "step_a must not re-run");
    assert_eq!(report.context.observed_by_b.as_deref(), Some("resv-42"));
}

#[tokio::test]
async fn forward_recovery_deduplicates_when_post_step_completion_write_fails() {
    let store = Arc::new(FaultySagaStore::new());
    // Reject the post-step completion write for step_a itself.
    store.fail_when(|r| {
        matches!(&r.status, SagaStatus::Running { current_step } if current_step == "step_a")
            && r.completed_steps == vec!["step_a".to_string()]
    });

    let applied_keys = Arc::new(Mutex::new(HashSet::<String>::new()));
    let reservations_created = Arc::new(AtomicUsize::new(0));

    let applied_c = Arc::clone(&applied_keys);
    let created_c = Arc::clone(&reservations_created);

    let saga = Saga::new("post_step_fail_dedup")
        .with_store(store.clone())
        .step_with_key(
            "step_a",
            move |_ctx: &mut (), key: String| {
                let applied = Arc::clone(&applied_c);
                let created = Arc::clone(&created_c);
                async move {
                    if applied.lock().expect("lock").insert(key) {
                        created.fetch_add(1, Ordering::SeqCst);
                    }
                    Ok(())
                }
            },
            |_, _| async { Ok(()) },
        )
        .step("step_b", |_| async { Ok(()) }, |_| async { Ok(()) });

    let saga_id = "saga-post-step-fail-1";
    let err = saga
        .run_with_id(saga_id, ())
        .await
        .err()
        .expect("post-step completion write must fail");
    assert_eq!(err.saga_id(), Some(saga_id));
    assert_eq!(reservations_created.load(Ordering::SeqCst), 1);

    // Even with ordinary `resume` (simulating a crash where the handle was lost),
    // the stable idempotency key prevents duplicating the reservation.
    store.clear_failure();
    let report = saga.resume(saga_id, ()).await.expect("resume must succeed");
    assert!(report.status.is_success());
    assert_eq!(
        reservations_created.load(Ordering::SeqCst),
        1,
        "reservation must not be duplicated on resume"
    );
}

#[tokio::test]
async fn rollback_intent_preserved_when_first_compensating_write_fails() {
    let store = Arc::new(FaultySagaStore::new());
    // Reject the very first Compensating write.
    store.fail_when(|r| matches!(&r.status, SagaStatus::Compensating { .. }));

    let step_b_attempts = Arc::new(AtomicUsize::new(0));
    let step_b_attempts_c = Arc::clone(&step_b_attempts);
    let step_a_compensated = Arc::new(AtomicUsize::new(0));
    let step_a_comp_c = Arc::clone(&step_a_compensated);

    let original_err = SagaError::ActionFailed {
        step: "step_b".into(),
        message: "payment declined".into(),
    };
    let original_err_c = original_err.clone();

    let saga = Saga::new("rollback_intent")
        .with_store(store.clone())
        .step(
            "step_a",
            |_| async { Ok(()) },
            move |_| {
                step_a_comp_c.fetch_add(1, Ordering::SeqCst);
                async { Ok(()) }
            },
        )
        .step(
            "step_b",
            move |_| {
                let attempt = step_b_attempts_c.fetch_add(1, Ordering::SeqCst);
                let err = original_err_c.clone();
                async move {
                    if attempt == 0 {
                        Err(err)
                    } else {
                        // Would succeed if erroneously re-run!
                        Ok(())
                    }
                }
            },
            |_| async { Ok(()) },
        );

    let err = saga
        .run_recoverable(())
        .await
        .err()
        .expect("first Compensating write must fail");
    assert!(matches!(
        err.handle.phase,
        SagaRecoveryPhase::Compensating {
            error: Some(ref e),
            ref compensation_errors,
        } if *e == original_err && compensation_errors.is_empty()
    ));

    store.clear_failure();
    let report = saga
        .resume_from_handle(*err.handle)
        .await
        .expect("resume_from_handle must complete rollback");
    assert_eq!(
        report.status,
        SagaStatus::Compensated {
            error: original_err
        }
    );
    assert_eq!(
        step_b_attempts.load(Ordering::SeqCst),
        1,
        "step_b must not be re-executed after it already failed"
    );
    assert_eq!(step_a_compensated.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn compensation_recovery_deduplicates_when_post_comp_write_fails() {
    let store = Arc::new(FaultySagaStore::new());
    // Reject the post-compensation completion write immediately after step_b's undo succeeds.
    store.fail_when(|r| {
        matches!(
            &r.status,
            SagaStatus::Compensating { current_step, .. } if current_step == "step_b"
        ) && r.compensated_steps == vec!["step_b".to_string()]
    });

    let comp_keys = Arc::new(Mutex::new(HashSet::<String>::new()));
    let b_releases = Arc::new(AtomicUsize::new(0));
    let a_releases = Arc::new(AtomicUsize::new(0));

    let comp_keys_c = Arc::clone(&comp_keys);
    let b_releases_c = Arc::clone(&b_releases);
    let a_releases_c = Arc::clone(&a_releases);

    let trigger_err = SagaError::ActionFailed {
        step: "step_c".into(),
        message: "boom".into(),
    };
    let trigger_err_c = trigger_err.clone();

    let saga = Saga::new("post_comp_fail_dedup")
        .with_store(store.clone())
        .step(
            "step_a",
            |_| async { Ok(()) },
            move |_| {
                a_releases_c.fetch_add(1, Ordering::SeqCst);
                async { Ok(()) }
            },
        )
        .step_with_key(
            "step_b",
            |_, _| async { Ok(()) },
            move |_, key: String| {
                let keys = Arc::clone(&comp_keys_c);
                let releases = Arc::clone(&b_releases_c);
                async move {
                    if keys.lock().expect("lock").insert(key) {
                        releases.fetch_add(1, Ordering::SeqCst);
                    }
                    Ok(())
                }
            },
        )
        .step(
            "step_c",
            move |_| {
                let e = trigger_err_c.clone();
                async move { Err(e) }
            },
            |_| async { Ok(()) },
        );

    let saga_id = "saga-post-comp-fail-1";
    let err = saga
        .run_with_id(saga_id, ())
        .await
        .err()
        .expect("post-compensation write must fail");
    assert_eq!(err.saga_id(), Some(saga_id));
    assert_eq!(b_releases.load(Ordering::SeqCst), 1);

    // Even with ordinary `resume` (simulating crash recovery), the stable compensation key
    // prevents releasing step_b's reservation twice.
    store.clear_failure();
    let report = saga
        .resume(saga_id, ())
        .await
        .expect("resume compensation must succeed");
    assert_eq!(
        report.status,
        SagaStatus::Compensated { error: trigger_err }
    );
    assert_eq!(
        b_releases.load(Ordering::SeqCst),
        1,
        "step_b undo must not be applied twice"
    );
    assert_eq!(a_releases.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn compensate_crashed_rejects_success_and_preserves_compensated() {
    let store = Arc::new(InMemorySagaStore::new());
    let comp_runs = Arc::new(AtomicUsize::new(0));
    let comp_runs_c = Arc::clone(&comp_runs);

    let saga = Saga::<()>::new("order").with_store(store.clone()).step(
        "reserve",
        |_| async { Ok(()) },
        move |_| {
            comp_runs_c.fetch_add(1, Ordering::SeqCst);
            async { Ok(()) }
        },
    );

    saga.run_with_id("s-ok", ()).await.unwrap();
    let err = saga.compensate_crashed("s-ok", ()).await.err().unwrap();
    assert!(matches!(err, SagaError::Aborted { .. }));
    assert_eq!(comp_runs.load(Ordering::SeqCst), 0);
    assert_eq!(
        store.load_record("s-ok").await.unwrap().unwrap().status,
        SagaStatus::Success
    );
}

#[tokio::test]
async fn resume_rejects_mismatched_saga_name_and_unknown_step() {
    let store = Arc::new(InMemorySagaStore::new());
    store
        .save_record_cas(
            &SagaRecord {
                saga_id: "s-other".into(),
                saga_name: "other_saga".into(),
                status: SagaStatus::Running {
                    current_step: "step_1".into(),
                },
                completed_steps: vec!["step_1".into()],
                compensated_steps: vec![],
                updated_at: 1,
                version: 1,
            },
            0,
        )
        .await
        .unwrap();

    let saga = Saga::<()>::new("order").with_store(store.clone()).step(
        "step_1",
        |_| async { Ok(()) },
        |_| async { Ok(()) },
    );
    let err = saga.resume("s-other", ()).await.err().unwrap();
    assert!(matches!(err, SagaError::Aborted { .. }));

    store
        .save_record_cas(
            &SagaRecord {
                saga_id: "s-bad-step".into(),
                saga_name: "order".into(),
                status: SagaStatus::Running {
                    current_step: "ghost_step".into(),
                },
                completed_steps: vec!["ghost_step".into()],
                compensated_steps: vec![],
                updated_at: 1,
                version: 1,
            },
            0,
        )
        .await
        .unwrap();
    let err2 = saga.resume("s-bad-step", ()).await.err().unwrap();
    assert!(matches!(err2, SagaError::Aborted { .. }));
}

#[tokio::test]
async fn concurrent_resume_cas_allows_single_winner_and_retains_all_comp_errors() {
    let store = Arc::new(InMemorySagaStore::new().with_max_retained_terminal(1));
    let saga = Arc::new(
        Saga::<()>::new("dual_comp")
            .with_store(store.clone())
            .step(
                "s1",
                |_| async { Ok(()) },
                |_| async {
                    Err(SagaError::CompensationFailed {
                        step: "s1".into(),
                        message: "c1".into(),
                    })
                },
            )
            .step(
                "s2",
                |_| async { Ok(()) },
                |_| async {
                    Err(SagaError::CompensationFailed {
                        step: "s2".into(),
                        message: "c2".into(),
                    })
                },
            )
            .step(
                "s3",
                |_| async {
                    Err(SagaError::ActionFailed {
                        step: "s3".into(),
                        message: "boom".into(),
                    })
                },
                |_| async { Ok(()) },
            ),
    );

    let report = saga.run_with_id("s-dual", ()).await.unwrap();
    match report.status {
        SagaStatus::CompensationFailed {
            compensation_errors,
            ..
        } => assert_eq!(compensation_errors.len(), 2),
        other => panic!("expected CompensationFailed, got {other:?}"),
    }

    // Second terminal saga triggers max_retained_terminal(1) pruning.
    let _ = saga.run_with_id("s-dual-2", ()).await.unwrap();
    assert_eq!(store.records.read().await.len(), 1);
}

#[tokio::test]
async fn overlapping_resume_is_blocked_by_exclusive_execution_claim() {
    let store = Arc::new(InMemorySagaStore::new());
    store
        .save_record_cas(
            &SagaRecord {
                saga_id: "s-overlap".into(),
                saga_name: "overlap_saga".into(),
                status: SagaStatus::Running {
                    current_step: "step_2".into(),
                },
                completed_steps: vec!["step_1".into()],
                compensated_steps: vec![],
                updated_at: 1,
                version: 1,
            },
            0,
        )
        .await
        .unwrap();

    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let step2_runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let e_clone = Arc::clone(&entered);
    let r_clone = Arc::clone(&release);
    let runs_clone = Arc::clone(&step2_runs);

    let saga = Arc::new(
        Saga::<()>::new("overlap_saga")
            .with_store(store.clone())
            .step("step_1", |_| async { Ok(()) }, |_| async { Ok(()) })
            .step(
                "step_2",
                move |_| {
                    let entered = Arc::clone(&e_clone);
                    let release = Arc::clone(&r_clone);
                    let runs = Arc::clone(&runs_clone);
                    async move {
                        runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        entered.notify_one();
                        release.notified().await;
                        Ok(())
                    }
                },
                |_| async { Ok(()) },
            ),
    );

    let worker_a_saga = Arc::clone(&saga);
    let worker_a = tokio::spawn(async move { worker_a_saga.resume("s-overlap", ()).await });
    entered.notified().await;

    // While Worker A is paused inside `step_2`, Worker B must be rejected before entering the step.
    let worker_b_err = saga.resume("s-overlap", ()).await.err().unwrap();
    assert!(matches!(worker_b_err, SagaError::Storage { .. }));
    assert_eq!(step2_runs.load(std::sync::atomic::Ordering::SeqCst), 1);

    release.notify_one();
    let report = worker_a.await.unwrap().expect("worker A succeeds");
    assert_eq!(report.status, SagaStatus::Success);
    assert_eq!(step2_runs.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test]
async fn resume_from_handle_rejects_mismatched_saga_name_handle() {
    let store = Arc::new(InMemorySagaStore::new());
    store
        .save_record_cas(
            &SagaRecord {
                saga_id: "s-cross".into(),
                saga_name: "workflow_a".into(),
                status: SagaStatus::Running {
                    current_step: "step_2".into(),
                },
                completed_steps: vec!["step_1".into()],
                compensated_steps: vec![],
                updated_at: 1,
                version: 1,
            },
            0,
        )
        .await
        .unwrap();

    let workflow_b = Saga::<()>::new("workflow_b")
        .with_store(store.clone())
        .step("step_1", |_| async { Ok(()) }, |_| async { Ok(()) })
        .step("step_2", |_| async { Ok(()) }, |_| async { Ok(()) });

    let err = workflow_b
        .resume_recoverable("s-cross", ())
        .await
        .err()
        .unwrap();
    assert!(matches!(err.error, SagaError::Aborted { .. }));

    // Passing the rejected handle back into `workflow_b.resume_from_handle` must also fail
    // and must NOT overwrite `workflow_a`'s stored record.
    let err2 = workflow_b
        .resume_from_handle(*err.handle)
        .await
        .err()
        .unwrap();
    assert!(matches!(err2.error, SagaError::Aborted { .. }));
    let stored = store.load_record("s-cross").await.unwrap().unwrap();
    assert_eq!(stored.saga_name, "workflow_a");
    assert_eq!(stored.version, 1);
}

#[test]
fn dropping_pending_saga_future_outside_runtime_releases_in_memory_claim() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let store = Arc::new(InMemorySagaStore::new());
    let saga = Saga::<()>::new("drop_outside_rt")
        .with_store(store.clone())
        .step(
            "s1",
            |_| async {
                std::future::pending::<()>().await;
                Ok(())
            },
            |_| async { Ok(()) },
        );

    // Poll the saga future inside the runtime context so it acquires the execution claim
    // and parks inside `s1`, then leave the runtime context before dropping the future.
    let mut fut = Box::pin(saga.run_with_id("s-drop-sync", ()));
    {
        let _enter = rt.enter();
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        assert!(fut.as_mut().poll(&mut cx).is_pending());
    }
    // Drop outside any Tokio runtime context; synchronous release must clear the claim.
    drop(fut);

    // A subsequent recovery on the same saga_id must succeed without a stuck claim error.
    let saga_fast = Saga::<()>::new("drop_outside_rt").with_store(store).step(
        "s1",
        |_| async { Ok(()) },
        |_| async { Ok(()) },
    );
    let report = rt
        .block_on(saga_fast.resume("s-drop-sync", ()))
        .expect("claim must be released when future is dropped outside runtime");
    assert_eq!(report.status, SagaStatus::Success);
}

#[tokio::test]
async fn resume_from_handle_rejects_compensating_already_succeeded_saga() {
    let store = Arc::new(InMemorySagaStore::new());
    let comp_runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let comp_clone = Arc::clone(&comp_runs);
    let workflow_a = Saga::<()>::new("workflow_a")
        .with_store(store.clone())
        .step(
            "step_1",
            |_| async { Ok(()) },
            move |_| {
                comp_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async { Ok(()) }
            },
        );

    let report = workflow_a.run_with_id("s-succeeded", ()).await.unwrap();
    assert_eq!(report.status, SagaStatus::Success);

    // Workflow B calls `compensate_crashed_recoverable` on workflow A's succeeded saga,
    // yielding a handle carrying workflow A's name, current version, and Compensating phase.
    let workflow_b = Saga::<()>::new("workflow_b")
        .with_store(store.clone())
        .step("step_1", |_| async { Ok(()) }, |_| async { Ok(()) });
    let mismatch_err = workflow_b
        .compensate_crashed_recoverable("s-succeeded", ())
        .await
        .err()
        .unwrap();
    assert_eq!(mismatch_err.handle.saga_name, "workflow_a");

    // Passing that handle to workflow A's `resume_from_handle` must reject compensation
    // and preserve `SagaStatus::Success`.
    let err = workflow_a
        .resume_from_handle(*mismatch_err.handle)
        .await
        .err()
        .unwrap();
    assert!(matches!(err.error, SagaError::Aborted { .. }));
    assert_eq!(comp_runs.load(std::sync::atomic::Ordering::SeqCst), 0);
    let stored = store.load_record("s-succeeded").await.unwrap().unwrap();
    assert_eq!(stored.status, SagaStatus::Success);
}

#[tokio::test]
async fn resume_from_handle_rejects_forward_handle_for_persisted_compensating_saga() {
    let store = Arc::new(InMemorySagaStore::new());
    store
        .save_record_cas(
            &SagaRecord {
                saga_id: "s-comp".into(),
                saga_name: "workflow_a".into(),
                status: SagaStatus::Compensating {
                    current_step: "step_1".into(),
                    error: Some(SagaError::ActionFailed {
                        step: "step_2".into(),
                        message: "boom".into(),
                    }),
                },
                completed_steps: vec!["step_1".into()],
                compensated_steps: vec![],
                updated_at: 1,
                version: 3,
            },
            0,
        )
        .await
        .unwrap();

    let step2_runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let runs_clone = Arc::clone(&step2_runs);
    let workflow_a = Saga::<()>::new("workflow_a")
        .with_store(store.clone())
        .step("step_1", |_| async { Ok(()) }, |_| async { Ok(()) })
        .step(
            "step_2",
            move |_| {
                runs_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async { Ok(()) }
            },
            |_| async { Ok(()) },
        );

    // Workflow B calls `resume_recoverable`, yielding an ownership-mismatch handle
    // carrying workflow A's name, current version, and `Forward` phase.
    let workflow_b = Saga::<()>::new("workflow_b")
        .with_store(store.clone())
        .step("step_1", |_| async { Ok(()) }, |_| async { Ok(()) });
    let mismatch_err = workflow_b
        .resume_recoverable("s-comp", ())
        .await
        .err()
        .unwrap();
    assert_eq!(mismatch_err.handle.saga_name, "workflow_a");
    assert!(matches!(
        mismatch_err.handle.phase,
        SagaRecoveryPhase::Forward
    ));

    // Workflow A must reject resuming forward execution on a persisted `Compensating` saga.
    let err = workflow_a
        .resume_from_handle(*mismatch_err.handle)
        .await
        .err()
        .unwrap();
    assert!(matches!(err.error, SagaError::Aborted { .. }));
    assert_eq!(step2_runs.load(std::sync::atomic::Ordering::SeqCst), 0);
}
