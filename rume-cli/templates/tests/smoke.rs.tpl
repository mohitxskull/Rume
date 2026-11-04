use rume::app::AppContext;
use rume::prelude::*;
use {{crate_name}}::app::{AppState, build_router};
use {{crate_name}}::config::Config;
use {{crate_name}}::env::Env;
use {{crate_name}}::migrations::migrator;
use {{crate_name}}::queries;
use {{crate_name}}::repository::{Item, ItemRepository};

#[test]
fn config_loads() {
    let config: Config = rume::config::load_for_env("test").expect("config should load");
    assert!(!config.app.name.is_empty());
}

#[tokio::test]
async fn health_endpoint_returns_ok() {
    let env = rume::env::load_for_env::<Env>("test").expect("env should load");
    let config = rume::config::load_for_env::<Config>("test").expect("config should load");
    let ctx = AppContext::builder(env, config).state(AppState).build();
    let router = build_router(ctx.clone()).with_state(ctx);
    let app = TestApp::new(router);

    let response = app.get("/health").await;
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let body = TestApp::body_text(response).await;
    assert!(body.contains("\"status\":\"ok\""));
}

#[test]
fn sql_assets_and_migrations_compile_and_load() {
    // Compile-time embedded SQL assets
    assert_eq!(queries::items::CREATE.name, "items.create");
    assert!(!queries::items::CREATE.sql.is_empty());
    assert!(!queries::items::FIND.sql.is_empty());
    assert!(!queries::items::LIST.sql.is_empty());

    // Conventional migrations chained with Rume core
    let pg_migrator = migrator("postgres");
    assert!(
        pg_migrator
            .migrations()
            .iter()
            .any(|m| m.id == "00000000000000_init")
    );
    assert!(
        pg_migrator
            .migrations()
            .iter()
            .any(|m| m.id.contains("rume_core"))
    );

    let sqlite_migrator = migrator("sqlite");
    assert!(
        sqlite_migrator
            .migrations()
            .iter()
            .any(|m| m.id == "00000000000000_init")
    );
    assert!(
        sqlite_migrator
            .migrations()
            .iter()
            .any(|m| m.id.contains("rume_core"))
    );
}

#[tokio::test]
async fn database_repository_operations_in_test_transaction() {
    let fixture = rume::testing::SqliteTestFixture::create()
        .await
        .expect("sqlite test fixture");

    let sqlite_migrator = migrator("sqlite");
    sqlite_migrator
        .run_pending(fixture.backend())
        .await
        .expect("migrations succeed");

    let tx = fixture.begin_test_tx().await.expect("begin test tx");

    let item = Item {
        id: "item_123".to_string(),
        name: "Test Scaffolding Item".to_string(),
        created_at: 1730700000,
    };

    ItemRepository::create_in_tx(tx.tx(), &item)
        .await
        .expect("create item");

    let found = ItemRepository::find_in_tx(tx.tx(), "item_123")
        .await
        .expect("find item");
    assert_eq!(found, Some(item.clone()));

    let list = ItemRepository::list_in_tx(tx.tx())
        .await
        .expect("list items");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0], item);
}
