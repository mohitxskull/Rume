//! Shared container fixtures with per-test database/schema isolation using `OnceCell`.

#![forbid(unsafe_code)]
#![allow(dead_code)]

use tokio::sync::OnceCell;

// ─── PostgreSQL Shared Container & Isolation ─────────────────────────────────

#[cfg(feature = "postgres")]
use testcontainers_modules::postgres::Postgres;
#[cfg(feature = "postgres")]
use testcontainers_modules::testcontainers::ImageExt;
#[cfg(feature = "postgres")]
use testcontainers_modules::testcontainers::runners::AsyncRunner;
#[cfg(feature = "postgres")]
use testcontainers_modules::testcontainers::ContainerAsync;

#[cfg(feature = "postgres")]
static POSTGRES_NODE: OnceCell<ContainerAsync<Postgres>> = OnceCell::const_new();

#[cfg(feature = "postgres")]
pub async fn shared_postgres() -> &'static ContainerAsync<Postgres> {
    POSTGRES_NODE
        .get_or_init(|| async {
            Postgres::default()
                .with_tag("16-alpine")
                .start()
                .await
                .expect("start shared postgres container")
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
    sqlx::query(&format!("CREATE DATABASE \"{db_name}\";"))
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
    config.connect_timeout_secs = 5;
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
            Mysql::default()
                .start()
                .await
                .expect("start shared mysql container")
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
    sqlx::query(&format!("CREATE DATABASE `{db_name}`;"))
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
            Redis::default()
                .with_tag("alpine")
                .start()
                .await
                .expect("start shared redis container")
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
    rume::cache::redis::RedisCacheBackend::new(&url).expect("create redis backend")
}
