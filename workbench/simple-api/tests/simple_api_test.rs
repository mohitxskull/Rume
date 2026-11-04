//! End-to-end qualification test for the Simple API application scenario.

use axum::http::StatusCode;
use rume::persistence::config::DatabaseConfig;
use rume::persistence::pool::DatabasePool;
use rume::persistence::sqlite::SqliteBackend;
use rume::testing::TestServer;
use serde_json::json;
use workbench_simple_api::{build_app, migrator, Item};

#[tokio::test]
async fn test_simple_api_lifecycle_validation_and_crud() {
    // 0. Verify Rume configuration system loads typed AppConfig from config/app.toml
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let app_cfg = workbench_simple_api::load_config(&manifest_dir).expect("load config from config/app.toml");
    assert_eq!(app_cfg.app_name, "simple-api");
    assert_eq!(app_cfg.port, 8080);

    let backend = SqliteBackend::in_memory()
        .await
        .expect("in-memory sqlite connects");
    let config = DatabaseConfig::new("memory://test");
    let pool = DatabasePool::new(backend, config);

    // Apply migrations
    let m = migrator();
    m.run_pending(pool.backend()).await.expect("migrations apply");

    let router = build_app(pool);
    let server = TestServer::start(router).await.expect("test server starts");

    // 1. Health check
    let health = server.get("/health").await.expect("health check");
    health.assert_status(StatusCode::OK).assert_text_equals("ok");

    // 2. Validation failure: title too short
    let invalid_body = json!({
        "title": "a", // min length is 3
        "price_cents": 500
    });
    let err_resp = server
        .post_json("/items", invalid_body.to_string())
        .await
        .expect("post invalid");
    err_resp.assert_status(StatusCode::UNPROCESSABLE_ENTITY);

    // 3. Validation failure: price 0 (min is 1)
    let invalid_price = json!({
        "title": "Valid Title",
        "price_cents": 0
    });
    let err_resp2 = server
        .post_json("/items", invalid_price.to_string())
        .await
        .expect("post invalid price");
    err_resp2.assert_status(StatusCode::UNPROCESSABLE_ENTITY);

    // 4. Create item successfully
    let valid_body = json!({
        "title": "Mechanical Keyboard",
        "description": "Ergonomic split keyboard with tactile switches",
        "price_cents": 14999
    });
    let create_resp = server
        .post_json("/items", valid_body.to_string())
        .await
        .expect("create item")
        .assert_status(StatusCode::CREATED);
    let created: Item = create_resp.json();
    assert_eq!(created.title, "Mechanical Keyboard");
    assert_eq!(created.price_cents, 14999);
    assert!(created.id.starts_with("item_"));

    // 5. Fetch item by ID
    let fetch_resp = server
        .get(&format!("/items/{}", created.id))
        .await
        .expect("fetch item")
        .assert_status(StatusCode::OK);
    let fetched: Item = fetch_resp.json();
    assert_eq!(fetched, created);

    // 6. List items
    let list_resp = server
        .get("/items")
        .await
        .expect("list items")
        .assert_status(StatusCode::OK);
    let items: Vec<Item> = list_resp.json();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0], created);

    // 7. Delete item
    let client = server.client();
    let delete_res = client
        .delete(server.url(&format!("/items/{}", created.id)))
        .send()
        .await
        .expect("delete item");
    assert_eq!(delete_res.status(), StatusCode::NO_CONTENT);

    // 8. Fetch deleted item returns 404
    let not_found = server
        .get(&format!("/items/{}", created.id))
        .await
        .expect("get deleted");
    not_found.assert_status(StatusCode::NOT_FOUND);

    server.shutdown().await.expect("server shuts down cleanly");
}
