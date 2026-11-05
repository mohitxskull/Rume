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

    let report = saga.run(initial).await;

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

    let report = saga.run(initial).await;

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
    let report = saga.run(initial).await;

    match report.status {
        SagaStatus::CompensationFailed {
            error,
            compensation_error,
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
    let report = saga.run(initial).await;

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
    let report = saga.run(initial).await;

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
    };
    store
        .save_record(&crashed_record)
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
        },
        completed_steps: vec!["step_1_reserve".into(), "step_2_charge".into()],
        compensated_steps: vec!["step_2_charge".into()],
        updated_at: 2000,
    };
    store
        .save_record(&crashed_record)
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
