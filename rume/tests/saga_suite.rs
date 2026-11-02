use rume::prelude::*;

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
