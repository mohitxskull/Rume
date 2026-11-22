use rume::saga::*;

#[derive(Default, Debug, Clone)]
struct OrderContext {
    order_id: String,
    inventory_reserved: bool,
    payment_processed: bool,
    shipment_created: bool,
    events: Vec<String>,
}

#[tokio::test]
async fn test_saga_happy_path() {
    let saga = Saga::new("order_fulfillment")
        .step(
            "reserve_inventory",
            |ctx: &mut OrderContext| {
                ctx.inventory_reserved = true;
                ctx.events.push("inventory_reserved".into());
                async { Ok(()) }
            },
            |ctx: &mut OrderContext| {
                ctx.inventory_reserved = false;
                ctx.events.push("inventory_released".into());
                async { Ok(()) }
            },
        )
        .step(
            "process_payment",
            |ctx: &mut OrderContext| {
                ctx.payment_processed = true;
                ctx.events.push("payment_processed".into());
                async { Ok(()) }
            },
            |ctx: &mut OrderContext| {
                ctx.payment_processed = false;
                ctx.events.push("payment_refunded".into());
                async { Ok(()) }
            },
        )
        .step(
            "create_shipment",
            |ctx: &mut OrderContext| {
                ctx.shipment_created = true;
                ctx.events.push("shipment_created".into());
                async { Ok(()) }
            },
            |ctx: &mut OrderContext| {
                ctx.shipment_created = false;
                ctx.events.push("shipment_cancelled".into());
                async { Ok(()) }
            },
        );

    let initial = OrderContext {
        order_id: "ord-101".into(),
        ..Default::default()
    };

    let report = saga.run(initial).await.expect("saga run should succeed");

    assert!(report.status.is_success());
    assert_eq!(report.context.order_id, "ord-101");
    assert_eq!(
        report.completed_steps,
        vec!["reserve_inventory", "process_payment", "create_shipment"]
    );
    assert!(report.compensated_steps.is_empty());
    assert!(report.context.inventory_reserved);
    assert!(report.context.payment_processed);
    assert!(report.context.shipment_created);
    assert_eq!(
        report.context.events,
        vec![
            "inventory_reserved",
            "payment_processed",
            "shipment_created"
        ]
    );
}

#[tokio::test]
async fn test_saga_compensation_on_failure() {
    let saga = Saga::new("order_fulfillment_failure")
        .step(
            "reserve_inventory",
            |ctx: &mut OrderContext| {
                ctx.inventory_reserved = true;
                ctx.events.push("inventory_reserved".into());
                async { Ok(()) }
            },
            |ctx: &mut OrderContext| {
                ctx.inventory_reserved = false;
                ctx.events.push("inventory_released".into());
                async { Ok(()) }
            },
        )
        .step(
            "process_payment",
            |ctx: &mut OrderContext| {
                ctx.payment_processed = true;
                ctx.events.push("payment_processed".into());
                async { Ok(()) }
            },
            |ctx: &mut OrderContext| {
                ctx.payment_processed = false;
                ctx.events.push("payment_refunded".into());
                async { Ok(()) }
            },
        )
        .step(
            "create_shipment",
            |_ctx: &mut OrderContext| async {
                Err(SagaError::ActionFailed {
                    step: "create_shipment".into(),
                    message: "courier API unavailable".into(),
                })
            },
            |ctx: &mut OrderContext| {
                ctx.shipment_created = false;
                async { Ok(()) }
            },
        );

    let initial = OrderContext {
        order_id: "ord-102".into(),
        ..Default::default()
    };

    let report = saga.run(initial).await.expect("saga run should succeed");

    assert!(!report.status.is_success());
    match report.status {
        SagaStatus::Compensated { error } => {
            assert_eq!(
                error,
                SagaError::ActionFailed {
                    step: "create_shipment".into(),
                    message: "courier API unavailable".into(),
                }
            );
        }
        _ => panic!("expected SagaStatus::Compensated"),
    }

    assert_eq!(
        report.completed_steps,
        vec!["reserve_inventory", "process_payment"]
    );
    // Compensations executed in reverse order: payment refunded first, then inventory released
    assert_eq!(
        report.compensated_steps,
        vec!["process_payment", "reserve_inventory"]
    );

    // Verify context reverted to unreserved and uncharged
    assert!(!report.context.inventory_reserved);
    assert!(!report.context.payment_processed);
    assert!(!report.context.shipment_created);
    assert_eq!(
        report.context.events,
        vec![
            "inventory_reserved",
            "payment_processed",
            "payment_refunded",
            "inventory_released"
        ]
    );
}

#[tokio::test]
async fn test_saga_compensation_failure() {
    let saga = Saga::new("order_compensation_failed")
        .step(
            "reserve_inventory",
            |ctx: &mut OrderContext| {
                ctx.inventory_reserved = true;
                async { Ok(()) }
            },
            |_ctx: &mut OrderContext| async {
                Err(SagaError::CompensationFailed {
                    step: "reserve_inventory".into(),
                    message: "warehouse database offline".into(),
                })
            },
        )
        .step(
            "process_payment",
            |_ctx: &mut OrderContext| async {
                Err(SagaError::ActionFailed {
                    step: "process_payment".into(),
                    message: "insufficient funds".into(),
                })
            },
            |_ctx: &mut OrderContext| async { Ok(()) },
        );

    let initial = OrderContext::default();
    let report = saga.run(initial).await.expect("saga run should succeed");

    match report.status {
        SagaStatus::CompensationFailed {
            error,
            compensation_error,
            ..
        } => {
            assert_eq!(
                error,
                SagaError::ActionFailed {
                    step: "process_payment".into(),
                    message: "insufficient funds".into(),
                }
            );
            assert_eq!(
                compensation_error,
                SagaError::CompensationFailed {
                    step: "reserve_inventory".into(),
                    message: "warehouse database offline".into(),
                }
            );
        }
        _ => panic!("expected SagaStatus::CompensationFailed"),
    }
}

#[tokio::test]
async fn test_saga_multi_step_partial_compensation_runs_all_steps() {
    let saga = Saga::new("multi_step_compensation")
        .step(
            "step_1",
            |ctx: &mut OrderContext| {
                ctx.events.push("step_1_done".into());
                async { Ok(()) }
            },
            |ctx: &mut OrderContext| {
                ctx.events.push("step_1_compensated".into());
                async { Ok(()) }
            },
        )
        .step(
            "step_2",
            |ctx: &mut OrderContext| {
                ctx.events.push("step_2_done".into());
                async { Ok(()) }
            },
            |_ctx: &mut OrderContext| async {
                Err(SagaError::CompensationFailed {
                    step: "step_2".into(),
                    message: "gateway down".into(),
                })
            },
        )
        .step(
            "step_3",
            |ctx: &mut OrderContext| {
                ctx.events.push("step_3_done".into());
                async { Ok(()) }
            },
            |ctx: &mut OrderContext| {
                ctx.events.push("step_3_compensated".into());
                async { Ok(()) }
            },
        )
        .step(
            "step_4_fails",
            |_ctx: &mut OrderContext| async {
                Err(SagaError::ActionFailed {
                    step: "step_4_fails".into(),
                    message: "terminal failure".into(),
                })
            },
            |_ctx: &mut OrderContext| async { Ok(()) },
        );

    let initial = OrderContext::default();
    let report = saga.run(initial).await.expect("saga run should succeed");

    // Even though step_2 compensation failed, step_3 AND step_1 compensations MUST run!
    assert!(matches!(
        report.status,
        SagaStatus::CompensationFailed { .. }
    ));
    assert!(
        report
            .context
            .events
            .contains(&"step_3_compensated".to_string())
    );
    assert!(
        report
            .context
            .events
            .contains(&"step_1_compensated".to_string())
    );
}

#[tokio::test]
async fn test_saga_step_panic_isolation() {
    let saga = Saga::new("panic_isolation_saga")
        .step(
            "step_1",
            |ctx: &mut OrderContext| {
                ctx.events.push("step_1_done".into());
                async { Ok(()) }
            },
            |ctx: &mut OrderContext| {
                ctx.events.push("step_1_compensated".into());
                async { Ok(()) }
            },
        )
        .step(
            "step_2_panics",
            |_ctx: &mut OrderContext| async {
                panic!("critical invariant broken in action!");
            },
            |_ctx: &mut OrderContext| async { Ok(()) },
        );

    let initial = OrderContext::default();
    let report = saga.run(initial).await.expect("saga run should succeed");

    // Panic must not crash thread; must compensate step 1
    assert!(matches!(report.status, SagaStatus::Compensated { .. }));
    assert!(
        report
            .context
            .events
            .contains(&"step_1_compensated".to_string())
    );
}

#[tokio::test]
async fn test_saga_persistence_and_crash_resume() {
    let store = std::sync::Arc::new(InMemorySagaStore::new());
    let saga_id = "saga-order-crash-recovery-1";

    let saga = Saga::new("persisted_order_flow")
        .with_store(store.clone())
        .step(
            "step_1_reserve",
            |ctx: &mut OrderContext| {
                ctx.events.push("step_1_done".into());
                async { Ok(()) }
            },
            |ctx: &mut OrderContext| {
                ctx.events.push("step_1_undone".into());
                async { Ok(()) }
            },
        )
        .step(
            "step_2_charge",
            |ctx: &mut OrderContext| {
                ctx.events.push("step_2_done".into());
                async { Ok(()) }
            },
            |ctx: &mut OrderContext| {
                ctx.events.push("step_2_undone".into());
                async { Ok(()) }
            },
        )
        .step(
            "step_3_ship",
            |ctx: &mut OrderContext| {
                ctx.events.push("step_3_done".into());
                async { Ok(()) }
            },
            |ctx: &mut OrderContext| {
                ctx.events.push("step_3_undone".into());
                async { Ok(()) }
            },
        );

    // 1. Simulate a crash after step 1 by directly setting a record with step 1 completed
    let crashed_record = SagaRecord {
        saga_id: saga_id.to_string(),
        saga_name: "persisted_order_flow".into(),
        status: SagaStatus::Running {
            current_step: "step_2_charge".into(),
        },
        completed_steps: vec!["step_1_reserve".into()],
        compensated_steps: Vec::new(),
        updated_at: 1000,
        version: 1,
    };
    store
        .save_record_cas(&crashed_record, 0)
        .await
        .expect("save crashed record");

    // 2. Query incomplete sagas - it must show up
    let incomplete = store.list_incomplete().await.expect("list incomplete");
    assert_eq!(incomplete.len(), 1);
    assert_eq!(incomplete[0].saga_id, saga_id);

    // 3. Resume the saga from the crash point
    let initial = OrderContext {
        order_id: "ord-resumed".into(),
        events: vec!["step_1_done".into()], // state left after step 1
        ..Default::default()
    };
    let report = saga.resume(saga_id, initial).await.expect("resume saga");

    assert!(report.status.is_success());
    assert_eq!(
        report.completed_steps,
        vec!["step_1_reserve", "step_2_charge", "step_3_ship"]
    );
    assert_eq!(
        report.context.events,
        vec!["step_1_done", "step_2_done", "step_3_done"]
    );

    // 4. Verify terminal record in store
    let persisted = store
        .load_record(saga_id)
        .await
        .expect("load")
        .expect("exists");
    assert_eq!(persisted.status, SagaStatus::Success);
    assert_eq!(persisted.completed_steps.len(), 3);

    // Incomplete list is now empty
    let incomplete_after = store.list_incomplete().await.expect("list");
    assert!(incomplete_after.is_empty());
}

#[tokio::test]
async fn test_saga_persistence_and_crashed_compensation() {
    let store = std::sync::Arc::new(InMemorySagaStore::new());
    let saga_id = "saga-order-crash-compensate-2";

    let saga = Saga::new("compensate_crashed_flow")
        .with_store(store.clone())
        .step(
            "step_1_reserve",
            |ctx: &mut OrderContext| {
                ctx.events.push("step_1_done".into());
                async { Ok(()) }
            },
            |ctx: &mut OrderContext| {
                ctx.events.push("step_1_undone".into());
                async { Ok(()) }
            },
        )
        .step(
            "step_2_charge",
            |ctx: &mut OrderContext| {
                ctx.events.push("step_2_done".into());
                async { Ok(()) }
            },
            |ctx: &mut OrderContext| {
                ctx.events.push("step_2_undone".into());
                async { Ok(()) }
            },
        );

    // Simulate coordinator crashed while compensating step 2 (step 2 already compensated, step 1 uncompensated)
    let crashed_record = SagaRecord {
        saga_id: saga_id.to_string(),
        saga_name: "compensate_crashed_flow".into(),
        status: SagaStatus::Compensating {
            current_step: "step_1_reserve".into(),
            error: None,
        },
        completed_steps: vec!["step_1_reserve".into(), "step_2_charge".into()],
        compensated_steps: vec!["step_2_charge".into()],
        updated_at: 2000,
        version: 1,
    };
    store
        .save_record_cas(&crashed_record, 0)
        .await
        .expect("save crashed record");

    let initial = OrderContext {
        order_id: "ord-compensate".into(),
        events: vec!["step_2_undone".into()],
        ..Default::default()
    };

    let report = saga
        .compensate_crashed(saga_id, initial)
        .await
        .expect("compensate crashed");

    assert!(matches!(report.status, SagaStatus::Compensated { .. }));
    // Step 1 was compensated during resume
    assert_eq!(
        report.compensated_steps,
        vec!["step_2_charge", "step_1_reserve"]
    );
    assert_eq!(
        report.context.events,
        vec!["step_2_undone", "step_1_undone"]
    );

    let persisted = store
        .load_record(saga_id)
        .await
        .expect("load")
        .expect("exists");
    assert!(matches!(persisted.status, SagaStatus::Compensated { .. }));
}

mod rc16_tests {
    use super::*;
    use rume::saga::coordinator::BoxFuture;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    type FailPredicate = Arc<dyn Fn(&SagaRecord) -> bool + Send + Sync>;

    #[derive(Clone, Default)]
    struct RecordingSagaStore {
        inner: InMemorySagaStore,
        saved: Arc<Mutex<Vec<SagaRecord>>>,
        fail_on: Arc<Mutex<Option<FailPredicate>>>,
    }

    impl RecordingSagaStore {
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

        fn recorded(&self) -> Vec<SagaRecord> {
            self.saved.lock().expect("lock").clone()
        }
    }

    impl SagaStore for RecordingSagaStore {
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
                        message: "injected save_record failure".into(),
                    })
                });
            }
            self.saved.lock().expect("lock").push(record.clone());
            self.inner.save_record_cas(record, expected_version)
        }

        fn load_record(
            &self,
            saga_id: &str,
        ) -> BoxFuture<'_, Result<Option<SagaRecord>, SagaError>> {
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

    #[test]
    #[should_panic(expected = "duplicate")]
    fn test_saga_rejects_duplicate_step_names_on_step() {
        let _saga = Saga::<OrderContext>::new("dup_steps")
            .step("same_name", |_| async { Ok(()) }, |_| async { Ok(()) })
            .step("same_name", |_| async { Ok(()) }, |_| async { Ok(()) });
    }

    #[test]
    #[should_panic(expected = "duplicate")]
    fn test_saga_rejects_duplicate_step_names_on_add_step() {
        let s1 =
            FunctionalStep::<OrderContext>::new("dup", |_| async { Ok(()) }, |_| async { Ok(()) });
        let s2 =
            FunctionalStep::<OrderContext>::new("dup", |_| async { Ok(()) }, |_| async { Ok(()) });
        let _saga = Saga::<OrderContext>::new("dup_add_step")
            .add_step(s1)
            .add_step(s2);
    }

    #[tokio::test]
    async fn test_saga_post_step_persist_and_no_reexecution_on_resume() {
        let store = Arc::new(RecordingSagaStore::new());
        // Fail save_record when transitioning to step_2 (simulates crash/failure between step 1 and step 2).
        store.fail_when(|r| {
            matches!(&r.status, SagaStatus::Running { current_step } if current_step == "step_2")
                || matches!(r.status, SagaStatus::Success)
        });

        let step_1_runs = Arc::new(AtomicUsize::new(0));
        let step_2_runs = Arc::new(AtomicUsize::new(0));
        let step_1_runs_c = Arc::clone(&step_1_runs);
        let step_2_runs_c = Arc::clone(&step_2_runs);

        let saga = Saga::new("post_step_persist_saga")
            .with_store(store.clone())
            .step(
                "step_1",
                move |ctx: &mut OrderContext| {
                    step_1_runs_c.fetch_add(1, Ordering::SeqCst);
                    ctx.events.push("step_1".into());
                    async { Ok(()) }
                },
                |_| async { Ok(()) },
            )
            .step(
                "step_2",
                move |ctx: &mut OrderContext| {
                    step_2_runs_c.fetch_add(1, Ordering::SeqCst);
                    ctx.events.push("step_2".into());
                    async { Ok(()) }
                },
                |_| async { Ok(()) },
            );

        let saga_id = "saga-post-step-1";
        let run_err = saga
            .run_with_id(saga_id, OrderContext::default())
            .await
            .err()
            .expect("expected Storage error when step_2 Running persist fails");
        assert!(matches!(run_err, SagaError::Storage { .. }));

        // Post-step persist must have saved step_1 as completed BEFORE step_2's Running transition.
        let recorded = store.recorded();
        assert!(
            recorded
                .iter()
                .any(|r| r.completed_steps == vec!["step_1".to_string()]),
            "expected post-step persist recording step_1 as completed, got: {recorded:?}"
        );

        // Now clear the store failure and resume: step_1 must NOT re-execute.
        store.clear_failure();
        let before_resume_step_1 = step_1_runs.load(Ordering::SeqCst);
        let report = saga
            .resume(saga_id, OrderContext::default())
            .await
            .expect("resume should succeed");
        assert!(report.status.is_success());
        assert_eq!(
            step_1_runs.load(Ordering::SeqCst),
            before_resume_step_1,
            "step_1 must not re-execute on resume"
        );
        assert_eq!(step_2_runs.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_saga_run_propagates_persist_state_error() {
        let store = Arc::new(RecordingSagaStore::new());
        store.fail_when(|_| true);

        let saga = Saga::new("run_persist_error_saga").with_store(store).step(
            "step_1",
            |_| async { Ok(()) },
            |_| async { Ok(()) },
        );

        let res = saga
            .run_with_id("saga-run-err-1", OrderContext::default())
            .await;
        assert!(
            matches!(res, Err(SagaError::Storage { .. })),
            "expected Storage error from persist_state to propagate from run_with_id"
        );
    }

    #[tokio::test]
    async fn test_saga_resume_propagates_persist_state_error() {
        let store = Arc::new(RecordingSagaStore::new());
        let saga = Saga::new("persist_error_saga")
            .with_store(store.clone())
            .step("step_1", |_| async { Ok(()) }, |_| async { Ok(()) });

        let saga_id = "saga-persist-err-1";
        store
            .save_record_cas(
                &SagaRecord {
                    saga_id: saga_id.into(),
                    saga_name: "persist_error_saga".into(),
                    status: SagaStatus::Pending,
                    completed_steps: Vec::new(),
                    compensated_steps: Vec::new(),
                    updated_at: 1000,
                    version: 1,
                },
                0,
            )
            .await
            .expect("seed record");

        store.fail_when(|_| true);
        let res = saga.resume(saga_id, OrderContext::default()).await;
        assert!(
            matches!(res, Err(SagaError::Storage { .. })),
            "expected Storage error from persist_state to propagate, got: {:?}",
            res.map(|r| r.status)
        );
    }

    #[tokio::test]
    async fn test_saga_resume_compensation_preserves_failure_reason() {
        let store = Arc::new(RecordingSagaStore::new());
        // Fail save_record when starting compensation for step_1 (simulating crash mid-compensation
        // after step_2 was compensated).
        store.fail_when(|r| {
            matches!(
                &r.status,
                SagaStatus::Compensating { current_step, .. } if current_step == "step_1"
            )
        });

        let expected_err = SagaError::ActionFailed {
            step: "step_3".into(),
            message: "payment gateway rejected".into(),
        };
        let expected_err_c = expected_err.clone();

        let saga = Saga::new("preserve_reason_saga")
            .with_store(store.clone())
            .step(
                "step_1",
                |ctx: &mut OrderContext| {
                    ctx.events.push("s1_done".into());
                    async { Ok(()) }
                },
                |ctx: &mut OrderContext| {
                    ctx.events.push("s1_comp".into());
                    async { Ok(()) }
                },
            )
            .step(
                "step_2",
                |ctx: &mut OrderContext| {
                    ctx.events.push("s2_done".into());
                    async { Ok(()) }
                },
                |ctx: &mut OrderContext| {
                    ctx.events.push("s2_comp".into());
                    async { Ok(()) }
                },
            )
            .step(
                "step_3",
                move |_| {
                    let e = expected_err_c.clone();
                    async move { Err(e) }
                },
                |_| async { Ok(()) },
            );

        let saga_id = "saga-preserve-reason-1";
        let run_err = saga
            .run_with_id(saga_id, OrderContext::default())
            .await
            .err()
            .expect("expected Storage error when step_1 Compensating persist fails");
        assert!(matches!(run_err, SagaError::Storage { .. }));

        let mid_crash = store
            .load_record(saga_id)
            .await
            .expect("load")
            .expect("exists");
        assert!(
            matches!(mid_crash.status, SagaStatus::Compensating { .. }),
            "expected saga to remain in Compensating after persist failure, got: {:?}",
            mid_crash.status
        );

        store.clear_failure();
        let report = saga
            .resume(saga_id, OrderContext::default())
            .await
            .expect("resume compensation should succeed");

        assert_eq!(
            report.status,
            SagaStatus::Compensated {
                error: expected_err
            }
        );
        // step_2 was already persisted as compensated before crash, so resume only compensates step_1.
        assert_eq!(report.context.events, vec!["s1_comp"]);
        assert_eq!(report.compensated_steps, vec!["step_2", "step_1"]);
    }

    #[tokio::test]
    async fn test_saga_resume_compensation_failed_preserves_failure_reason() {
        let store = Arc::new(RecordingSagaStore::new());
        store.fail_when(|r| {
            matches!(
                &r.status,
                SagaStatus::Compensating { current_step, .. } if current_step == "step_1"
            )
        });

        let expected_err = SagaError::ActionFailed {
            step: "step_3".into(),
            message: "inventory locked".into(),
        };
        let expected_comp_err = SagaError::CompensationFailed {
            step: "step_1".into(),
            message: "refund service down".into(),
        };
        let expected_err_c = expected_err.clone();
        let expected_comp_err_c = expected_comp_err.clone();

        let saga = Saga::new("preserve_reason_comp_failed_saga")
            .with_store(store.clone())
            .step(
                "step_1",
                |_| async { Ok(()) },
                move |_| {
                    let e = expected_comp_err_c.clone();
                    async move { Err(e) }
                },
            )
            .step("step_2", |_| async { Ok(()) }, |_| async { Ok(()) })
            .step(
                "step_3",
                move |_| {
                    let e = expected_err_c.clone();
                    async move { Err(e) }
                },
                |_| async { Ok(()) },
            );

        let saga_id = "saga-preserve-comp-failed-1";
        let _ = saga
            .run_with_id(saga_id, OrderContext::default())
            .await
            .err()
            .expect("expected Storage error when step_1 Compensating persist fails");

        store.clear_failure();
        let report = saga
            .resume(saga_id, OrderContext::default())
            .await
            .expect("resume compensation should return CompensationFailed report");

        assert_eq!(
            report.status,
            SagaStatus::CompensationFailed {
                error: expected_err,
                compensation_error: expected_comp_err.clone(),
                compensation_errors: vec![expected_comp_err],
            }
        );
    }
}
