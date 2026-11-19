//! Shared core, audit, and isolation SQL constants owned by `rume-data`.

// Why: Core schema DDL (`rume_jobs`, `rume_audit_log`), audit counts, and MySQL
// isolation-level SQL are single-owned here alongside `Migrator::with_rume_core` so
// downstream crates (`rume-messaging`, `rume-security`) consume them through Rust
// module exports rather than relative cross-crate `include_str!` paths that fail
// `cargo package`. Messaging-only operational job queries live inside `rume-messaging`.

/// Schema DDL for the durable `rume_jobs` table in PostgreSQL.
pub const PG_JOBS_SCHEMA: &str = include_str!("sql/postgres/jobs_schema.sql");

/// Schema DDL for the `rume_audit_log` table in PostgreSQL.
pub const PG_AUDIT_SCHEMA: &str = include_str!("sql/postgres/audit_schema.sql");

/// Schema DDL for the `rume_audit_log` table in MySQL.
pub const MYSQL_AUDIT_SCHEMA: &str = include_str!("sql/mysql/audit_schema.sql");

/// Schema DDL for the `rume_audit_log` table in SQLite.
pub const SQLITE_AUDIT_SCHEMA: &str = include_str!("sql/sqlite/audit_schema.sql");

/// Count query for total recorded audit logs.
pub const AUDIT_COUNT_SQL: &str = include_str!("sql/common/audit_count.sql");

/// Session isolation-level statement used before MySQL outbox claim transactions.
pub const MYSQL_SET_ISOLATION_LEVEL: &str = include_str!("sql/mysql/set_isolation_level.sql");
