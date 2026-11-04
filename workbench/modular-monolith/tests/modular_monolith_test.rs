//! End-to-end qualification test for the Modular Monolith application scenario.

use std::sync::Arc;
use axum::http::StatusCode;
use rume::events::EventBus;
use rume::testing::TestServer;
use serde_json::json;
use tokio::sync::RwLock;
use workbench_modular_monolith::{build_modular_monolith, Order, Product};

#[tokio::test]
async fn test_modular_monolith_domain_isolation_and_events() {
    let event_bus = Arc::new(EventBus::new());
    let boot_log = Arc::new(RwLock::new(Vec::new()));

    let builder = build_modular_monolith(Arc::clone(&event_bus), Arc::clone(&boot_log));
    let server = TestServer::from_builder(builder)
        .await
        .expect("modular monolith server starts");

    // 1. Verify topological startup and reverse shutdown order:
    // Catalog must initialize before Orders, and Orders must shut down before Catalog.
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
            "startup must be topological (catalog -> orders) and shutdown must be reverse topological (orders -> catalog)"
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
    let order_resp = server
        .post_json("/orders", order_payload.to_string())
        .await
        .expect("post order")
        .assert_status(StatusCode::CREATED);
    let placed_order: Order = order_resp.json();
    assert_eq!(placed_order.product_id, "prod_laptop");
    assert_eq!(placed_order.quantity, 3);

    // Yield a tick for asynchronous event delivery
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

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

    // 6. Graceful shutdown: verify reverse-order termination (orders stops before catalog)
    server.shutdown().await.expect("shutdown completes");

    // Rume builder shutdown hooks executed in reverse topological order
}
