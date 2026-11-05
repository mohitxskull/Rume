//! End-to-end qualification test for the Modular Monolith application scenario.

use axum::http::StatusCode;
use rume::events::EventBus;
use rume::testing::TestServer;
use serde_json::json;
use std::sync::Arc;
use tokio::sync::{Notify, RwLock};
use workbench_modular_monolith::{Order, Product, build_modular_monolith_instrumented};

#[tokio::test]
async fn test_modular_monolith_domain_isolation_and_events() {
    let event_bus = Arc::new(EventBus::new());
    let boot_log = Arc::new(RwLock::new(Vec::new()));
    let event_completion = Arc::new(Notify::new());

    let builder = build_modular_monolith_instrumented(
        Arc::clone(&event_bus),
        Arc::clone(&boot_log),
        Some(Arc::clone(&event_completion)),
    );
    let server = TestServer::from_builder(builder)
        .await
        .expect("modular monolith server starts");

    // 1. Verify topological startup order:
    // Catalog must initialize before Orders, and shutdown hooks MUST NOT have executed while server is active.
    {
        let logs = boot_log.read().await;
        assert_eq!(
            *logs,
            vec!["catalog:started", "orders:started"],
            "startup must be topological (catalog -> orders) and modules must remain running while server serves traffic"
        );
    }

    // 2. Query initial catalog state
    let initial_resp = server
        .get("/catalog/products/prod_laptop")
        .await
        .expect("get initial laptop")
        .assert_status(StatusCode::OK);
    let initial_product: Product = initial_resp.json();
    assert_eq!(initial_product.stock, 10);

    // 3. Place an order through the orders domain
    let order_payload = json!({
        "product_id": "prod_laptop",
        "quantity": 3
    });
    let notified = event_completion.notified();
    let order_resp = server
        .post_json("/orders", order_payload.to_string())
        .await
        .expect("post order")
        .assert_status(StatusCode::CREATED);
    let placed_order: Order = order_resp.json();
    assert_eq!(placed_order.product_id, "prod_laptop");
    assert_eq!(placed_order.quantity, 3);

    // Deterministic synchronization: await event delivery notification from catalog domain
    tokio::time::timeout(std::time::Duration::from_secs(5), notified)
        .await
        .expect("event delivery notification received within 5s");

    // 4. Verify catalog domain automatically updated inventory in response to the decoupled event
    let updated_resp = server
        .get("/catalog/products/prod_laptop")
        .await
        .expect("get updated laptop")
        .assert_status(StatusCode::OK);
    let updated_product: Product = updated_resp.json();
    assert_eq!(
        updated_product.stock, 7,
        "catalog inventory should be automatically decremented from 10 to 7 via decoupled event"
    );

    // 5. Query order list
    let orders_list_resp = server
        .get("/orders")
        .await
        .expect("list orders")
        .assert_status(StatusCode::OK);
    let orders: Vec<Order> = orders_list_resp.json();
    assert_eq!(orders.len(), 1);
    assert_eq!(orders[0], placed_order);

    // Verify modules are STILL running before shutdown is called
    {
        let logs = boot_log.read().await;
        assert_eq!(
            *logs,
            vec!["catalog:started", "orders:started"],
            "modules must still be running before server shutdown"
        );
    }

    // 6. Graceful shutdown: drains TCP connections and triggers reverse-order termination (orders stops before catalog)
    server.shutdown().await.expect("shutdown completes");

    // Verify reverse topological shutdown hooks executed only after server shutdown
    {
        let logs = boot_log.read().await;
        assert_eq!(
            *logs,
            vec![
                "catalog:started",
                "orders:started",
                "orders:stopped",
                "catalog:stopped"
            ],
            "shutdown must be reverse topological (orders -> catalog) and run only upon server shutdown"
        );
    }
}
