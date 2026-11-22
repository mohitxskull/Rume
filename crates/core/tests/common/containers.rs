//! Shared container fixtures with per-test database/schema isolation using `OnceCell`.

#![forbid(unsafe_code)]
#![allow(dead_code)]

#[cfg(any(feature = "postgres", feature = "mysql"))]
use sqlx::AssertSqlSafe;
#[cfg(any(feature = "postgres", feature = "redis"))]
use testcontainers_modules::testcontainers::ImageExt;
#[cfg(any(feature = "postgres", feature = "mysql", feature = "redis"))]
use testcontainers_modules::testcontainers::{ContainerAsync, runners::AsyncRunner};
#[cfg(any(feature = "postgres", feature = "mysql", feature = "redis"))]
use tokio::sync::OnceCell;

/// Spawns a detached OS process that removes `container_id` as soon as this test binary exits.
///
/// Why: `POSTGRES_NODE`, `MYSQL_NODE`, and `REDIS_NODE` live in `static OnceCell`s, and Rust never
/// runs `Drop` on `static` items at process exit. Without an out-of-process reaper, every run of
/// `cargo test -p rume --test containers` permanently leaks a running Docker container (even on
/// normal exit, panic, or `SIGKILL`), eventually exhausting host RAM and swap.
#[cfg(any(feature = "postgres", feature = "mysql", feature = "redis"))]
fn spawn_container_reaper(container_id: &str) {
    let pid = std::process::id();
    #[cfg(unix)]
    {
        let _ = std::process::Command::new("sh")
            .args([
                "-c",
                "while kill -0 \"$1\" 2>/dev/null; do sleep 1; done; docker rm -f \"$2\" >/dev/null 2>&1",
                "rume-container-reaper",
                &pid.to_string(),
                container_id,
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
    #[cfg(not(unix))]
    {
        let _ = (pid, container_id);
    }
}

// ─── PostgreSQL Shared Container & Isolation ─────────────────────────────────

#[cfg(feature = "postgres")]
use testcontainers_modules::postgres::Postgres;

#[cfg(feature = "postgres")]
static POSTGRES_NODE: OnceCell<ContainerAsync<Postgres>> = OnceCell::const_new();

#[cfg(feature = "postgres")]
pub async fn shared_postgres() -> &'static ContainerAsync<Postgres> {
    POSTGRES_NODE
        .get_or_init(|| async {
            let container = Postgres::default()
                .with_tag("16-alpine")
                .start()
                .await
                .expect("start shared postgres container");
            spawn_container_reaper(container.id());
            container
        })
        .await
}

#[cfg(feature = "postgres")]
pub async fn create_isolated_postgres_db() -> (String, rume::persistence::config::DatabaseConfig) {
    let node = shared_postgres().await;
    let host = node.get_host().await.expect("host");
    let port = node.get_host_port_ipv4(5432).await.expect("port");
    let db_name = format!("test_{}", uuid::Uuid::new_v4().simple());

    let root_url = format!("postgres://postgres:postgres@{host}:{port}/postgres");
    let root_pool = sqlx::PgPool::connect(&root_url)
        .await
        .expect("connect root postgres");
    sqlx::query(AssertSqlSafe(format!("CREATE DATABASE \"{db_name}\";")))
        .execute(&root_pool)
        .await
        .expect("create test db");

    let test_url = format!("postgres://postgres:postgres@{host}:{port}/{db_name}");
    let config = rume::persistence::config::DatabaseConfig::new(&test_url);
    (db_name, config)
}

#[cfg(feature = "postgres")]
pub async fn create_isolated_postgres_pool(
    max_connections: u32,
) -> (
    String,
    rume::persistence::pool::DatabasePool<rume::persistence::postgres::PostgresBackend>,
) {
    let (db_name, mut config) = create_isolated_postgres_db().await;
    config.max_connections = max_connections;
    config.min_connections = 1;
    // Generous bound: under loaded shared-container CI, pool acquisition can
    // take longer than 5s with ~84 parallel tests contending for connections.
    config.connect_timeout_secs = 30;
    config.idle_timeout_secs = Some(10);
    let backend = rume::persistence::postgres::PostgresBackend::new(&config)
        .expect("create postgres backend");
    let pool = rume::persistence::pool::DatabasePool::new(backend, config);
    (db_name, pool)
}

// ─── MySQL Shared Container & Isolation ──────────────────────────────────────

#[cfg(feature = "mysql")]
use testcontainers_modules::mysql::Mysql;

#[cfg(feature = "mysql")]
static MYSQL_NODE: OnceCell<ContainerAsync<Mysql>> = OnceCell::const_new();

#[cfg(feature = "mysql")]
pub async fn shared_mysql() -> &'static ContainerAsync<Mysql> {
    MYSQL_NODE
        .get_or_init(|| async {
            let container = Mysql::default()
                .start()
                .await
                .expect("start shared mysql container");
            spawn_container_reaper(container.id());
            container
        })
        .await
}

#[cfg(feature = "mysql")]
pub async fn create_isolated_mysql_db() -> (String, rume::persistence::config::DatabaseConfig) {
    let node = shared_mysql().await;
    let host = node.get_host().await.expect("host");
    let port = node.get_host_port_ipv4(3306).await.expect("port");
    let db_name = format!("test_{}", uuid::Uuid::new_v4().simple());

    let root_url = format!("mysql://root@{host}:{port}/mysql");
    let root_pool = sqlx::MySqlPool::connect(&root_url)
        .await
        .expect("connect root mysql");
    sqlx::query(AssertSqlSafe(format!("CREATE DATABASE `{db_name}`;")))
        .execute(&root_pool)
        .await
        .expect("create mysql test db");

    let test_url = format!("mysql://root@{host}:{port}/{db_name}");
    let config = rume::persistence::config::DatabaseConfig::new(&test_url);
    (db_name, config)
}

#[cfg(feature = "mysql")]
pub async fn create_isolated_mysql_pool(
    max_connections: u32,
) -> (
    String,
    rume::persistence::pool::DatabasePool<rume::persistence::mysql::MysqlBackend>,
) {
    let (db_name, mut config) = create_isolated_mysql_db().await;
    config.max_connections = max_connections;
    config.min_connections = 1;
    let backend =
        rume::persistence::mysql::MysqlBackend::new(&config).expect("create mysql backend");
    let pool = rume::persistence::pool::DatabasePool::new(backend, config);
    (db_name, pool)
}

// ─── Redis Shared Container & Isolation ──────────────────────────────────────

#[cfg(feature = "redis")]
use testcontainers_modules::redis::{REDIS_PORT, Redis};

#[cfg(feature = "redis")]
static REDIS_NODE: OnceCell<ContainerAsync<Redis>> = OnceCell::const_new();

#[cfg(feature = "redis")]
pub async fn shared_redis() -> &'static ContainerAsync<Redis> {
    REDIS_NODE
        .get_or_init(|| async {
            let container = Redis::default()
                .with_tag("alpine")
                .start()
                .await
                .expect("start shared redis container");
            spawn_container_reaper(container.id());
            container
        })
        .await
}

#[cfg(feature = "redis")]
pub async fn shared_redis_url() -> String {
    let node = shared_redis().await;
    let host = node.get_host().await.expect("host");
    let port = node.get_host_port_ipv4(REDIS_PORT).await.expect("port");
    format!("redis://{host}:{port}")
}

#[cfg(feature = "redis")]
pub async fn create_isolated_redis_pool() -> deadpool_redis::Pool {
    let url = shared_redis_url().await;
    let pool_cfg = deadpool_redis::Config::from_url(&url);
    pool_cfg
        .create_pool(Some(deadpool_redis::Runtime::Tokio1))
        .expect("create redis pool")
}

#[cfg(feature = "redis")]
pub async fn create_isolated_redis_backend() -> rume::cache::redis::RedisCacheBackend {
    let url = shared_redis_url().await;
    let prefix = format!("rume:cache:{}:", uuid::Uuid::new_v4().simple());
    rume::cache::redis::RedisCacheBackend::new(&url)
        .expect("create redis backend")
        .with_prefix(prefix)
}
