//! Single-binary executable entry point for the Simple API scenario.

use std::path::Path;
use rume::persistence::config::DatabaseConfig;
use rume::persistence::pool::DatabasePool;
use rume::persistence::sqlite::SqliteBackend;
use workbench_simple_api::{build_app, load_config, migrator};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // 1. Load application configuration through Rume's configuration system
    let config = load_config(Path::new(".")).unwrap_or_default();
    println!("Starting {} on port {}", config.app_name, config.port);

    // 2. Initialize persistence
    let db_config = DatabaseConfig::new(&config.database_url).with_max_connections(5);
    let backend = SqliteBackend::new(&db_config).await?;
    let pool = DatabasePool::new(backend, db_config);

    // 3. Execute embedded migrations
    migrator().run_pending(pool.backend()).await?;

    // 4. Build application router
    let app = build_app(pool);

    // 5. Run HTTP server
    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", config.port)).await?;
    println!("Listening on http://{}", listener.local_addr()?);
    axum::serve(listener, app).await?;

    Ok(())
}
