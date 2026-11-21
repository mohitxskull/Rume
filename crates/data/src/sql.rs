//! Shared SQL constants owned by `rume-data`.
//!
//! Why: All SQL assets live in the single tree at `crates/data/sql/`, and every
//! consumer reaches them through Rust module exports rather than relative
//! cross-crate `include_str!` paths that fail `cargo package`. Downstream crates
//! (`rume-messaging`, `rume-security`) consume these consts via `rume_data::sql`.

// Moved from the deleted `crates/data/src/persistence/sql.rs`.

/// Schema DDL for the durable `rume_jobs` table in PostgreSQL.
pub const PG_JOBS_SCHEMA: &str = include_str!("../sql/postgres/jobs_schema.sql");

/// Schema DDL for the `rume_audit_log` table in PostgreSQL.
pub const PG_AUDIT_SCHEMA: &str = include_str!("../sql/postgres/audit_schema.sql");

/// Schema DDL for the `rume_audit_log` table in MySQL.
pub const MYSQL_AUDIT_SCHEMA: &str = include_str!("../sql/mysql/audit_schema.sql");

/// Schema DDL for the `rume_audit_log` table in SQLite.
pub const SQLITE_AUDIT_SCHEMA: &str = include_str!("../sql/sqlite/audit_schema.sql");

/// Count query for total recorded audit logs.
pub const AUDIT_COUNT_SQL: &str = include_str!("../sql/common/audit_count.sql");

/// Session isolation-level statement used before MySQL outbox claim transactions.
pub const MYSQL_SET_ISOLATION_LEVEL: &str = include_str!("../sql/mysql/set_isolation_level.sql");

// Jobs (PostgreSQL), moved from `crates/messaging/src/jobs/sql/postgres/`.

/// Claim the next batch of pending jobs.
pub const PG_JOBS_CLAIM: &str = include_str!("../sql/postgres/jobs_claim.sql");
/// Insert a new durable job.
pub const PG_JOBS_INSERT: &str = include_str!("../sql/postgres/jobs_insert.sql");
/// Reap jobs abandoned by dead workers.
pub const PG_JOBS_REAP: &str = include_str!("../sql/postgres/jobs_reap.sql");
/// Count currently pending jobs.
pub const PG_JOBS_PENDING_COUNT: &str = include_str!("../sql/postgres/jobs_pending_count.sql");
/// Count jobs in the dead-letter queue.
pub const PG_JOBS_DEAD_LETTER_COUNT: &str =
    include_str!("../sql/postgres/jobs_dead_letter_count.sql");
/// Check for an existing idempotency key.
pub const PG_JOBS_IDEMPOTENCY_CHECK: &str =
    include_str!("../sql/postgres/jobs_idempotency_check.sql");
/// Acknowledge a claimed job.
pub const PG_JOBS_ACK: &str = include_str!("../sql/postgres/jobs_ack.sql");
/// Negatively acknowledge a claimed job.
pub const PG_JOBS_NACK: &str = include_str!("../sql/postgres/jobs_nack.sql");
/// Defer a job until a later time.
pub const PG_JOBS_DEFER: &str = include_str!("../sql/postgres/jobs_defer.sql");
/// Move a job to the dead-letter queue.
pub const PG_JOBS_DEAD_LETTER: &str = include_str!("../sql/postgres/jobs_dead_letter.sql");
/// Renew the lease of an in-flight job.
pub const PG_JOBS_RENEW_LEASE: &str = include_str!("../sql/postgres/jobs_renew_lease.sql");
/// List dead-lettered jobs.
pub const PG_JOBS_LIST_DEAD_LETTERS: &str =
    include_str!("../sql/postgres/jobs_list_dead_letters.sql");
/// Replay a dead-lettered job.
pub const PG_JOBS_REPLAY_DEAD_LETTER: &str =
    include_str!("../sql/postgres/jobs_replay_dead_letter.sql");
/// Purge terminal jobs.
pub const PG_JOBS_PURGE: &str = include_str!("../sql/postgres/jobs_purge.sql");

// Outbox (MySQL), moved from `crates/messaging/src/outbox/sql/mysql/`.

/// Claim pending outbox messages.
pub const MYSQL_OUTBOX_CLAIM: &str = include_str!("../sql/mysql/outbox_claim.sql");
/// Fetch pending messages without claiming them.
pub const MYSQL_OUTBOX_FETCH_PENDING: &str = include_str!("../sql/mysql/outbox_fetch_pending.sql");
/// Insert a new outbox message.
pub const MYSQL_OUTBOX_INSERT: &str = include_str!("../sql/mysql/outbox_insert.sql");
/// Recover outbox messages with expired leases.
pub const MYSQL_OUTBOX_RECOVER: &str = include_str!("../sql/mysql/outbox_recover.sql");
/// Claim pending messages with an explicit lease.
pub const MYSQL_OUTBOX_CLAIM_LEASE: &str = include_str!("../sql/mysql/outbox_claim_lease.sql");
/// Select the lease expiry of a claimed message.
pub const MYSQL_OUTBOX_SELECT_LEASE_EXPIRES_AT: &str =
    include_str!("../sql/mysql/outbox_select_lease_expires_at.sql");
/// Renew the lease of a claimed message.
pub const MYSQL_OUTBOX_RENEW_LEASE: &str = include_str!("../sql/mysql/outbox_renew_lease.sql");
/// Mark a message as published.
pub const MYSQL_OUTBOX_MARK_PUBLISHED: &str =
    include_str!("../sql/mysql/outbox_mark_published.sql");
/// Select the retry count of a message.
pub const MYSQL_OUTBOX_SELECT_RETRY_COUNT: &str =
    include_str!("../sql/mysql/outbox_select_retry_count.sql");
/// Mark a message as failed.
pub const MYSQL_OUTBOX_MARK_FAILED: &str = include_str!("../sql/mysql/outbox_mark_failed.sql");
/// Count pending messages.
pub const MYSQL_OUTBOX_PENDING_COUNT: &str = include_str!("../sql/mysql/outbox_pending_count.sql");
/// Count published messages.
pub const MYSQL_OUTBOX_PUBLISHED_COUNT: &str =
    include_str!("../sql/mysql/outbox_published_count.sql");
/// Count failed messages.
pub const MYSQL_OUTBOX_FAILED_COUNT: &str = include_str!("../sql/mysql/outbox_failed_count.sql");
/// Fetch failed messages.
pub const MYSQL_OUTBOX_FETCH_FAILED: &str = include_str!("../sql/mysql/outbox_fetch_failed.sql");
/// Re-process a failed message.
pub const MYSQL_OUTBOX_REPROCESS_FAILED: &str =
    include_str!("../sql/mysql/outbox_reprocess_failed.sql");
/// Purge published messages.
pub const MYSQL_OUTBOX_PURGE_PUBLISHED: &str =
    include_str!("../sql/mysql/outbox_purge_published.sql");

// Outbox (PostgreSQL), moved from `crates/messaging/src/outbox/sql/postgres/`.

/// Claim pending outbox messages.
pub const PG_OUTBOX_CLAIM: &str = include_str!("../sql/postgres/outbox_claim.sql");
/// Fetch pending messages without claiming them.
pub const PG_OUTBOX_FETCH_PENDING: &str = include_str!("../sql/postgres/outbox_fetch_pending.sql");
/// Insert a new outbox message.
pub const PG_OUTBOX_INSERT: &str = include_str!("../sql/postgres/outbox_insert.sql");
/// Recover outbox messages with expired leases.
pub const PG_OUTBOX_RECOVER: &str = include_str!("../sql/postgres/outbox_recover.sql");
/// Claim pending messages with an explicit lease.
pub const PG_OUTBOX_CLAIM_LEASE: &str = include_str!("../sql/postgres/outbox_claim_lease.sql");
/// Renew the lease of a claimed message.
pub const PG_OUTBOX_RENEW_LEASE: &str = include_str!("../sql/postgres/outbox_renew_lease.sql");
/// Mark a message as published.
pub const PG_OUTBOX_MARK_PUBLISHED: &str =
    include_str!("../sql/postgres/outbox_mark_published.sql");
/// Select the retry count of a message.
pub const PG_OUTBOX_SELECT_RETRY_COUNT: &str =
    include_str!("../sql/postgres/outbox_select_retry_count.sql");
/// Mark a message as failed.
pub const PG_OUTBOX_MARK_FAILED: &str = include_str!("../sql/postgres/outbox_mark_failed.sql");
/// Count pending messages.
pub const PG_OUTBOX_PENDING_COUNT: &str = include_str!("../sql/postgres/outbox_pending_count.sql");
/// Count published messages.
pub const PG_OUTBOX_PUBLISHED_COUNT: &str =
    include_str!("../sql/postgres/outbox_published_count.sql");
/// Count failed messages.
pub const PG_OUTBOX_FAILED_COUNT: &str = include_str!("../sql/postgres/outbox_failed_count.sql");
/// Fetch failed messages.
pub const PG_OUTBOX_FETCH_FAILED: &str = include_str!("../sql/postgres/outbox_fetch_failed.sql");
/// Re-process a failed message.
pub const PG_OUTBOX_REPROCESS_FAILED: &str =
    include_str!("../sql/postgres/outbox_reprocess_failed.sql");
/// Purge published messages.
pub const PG_OUTBOX_PURGE_PUBLISHED: &str =
    include_str!("../sql/postgres/outbox_purge_published.sql");

// Outbox (SQLite), moved from `crates/messaging/src/outbox/sql/sqlite/`.

/// Claim pending outbox messages.
pub const SQLITE_OUTBOX_CLAIM: &str = include_str!("../sql/sqlite/outbox_claim.sql");
/// Insert a new outbox message.
pub const SQLITE_OUTBOX_INSERT: &str = include_str!("../sql/sqlite/outbox_insert.sql");
/// Recover outbox messages with expired leases.
pub const SQLITE_OUTBOX_RECOVER: &str = include_str!("../sql/sqlite/outbox_recover.sql");
/// Claim pending messages with an explicit lease.
pub const SQLITE_OUTBOX_CLAIM_LEASE: &str = include_str!("../sql/sqlite/outbox_claim_lease.sql");
/// Renew the lease of a claimed message.
pub const SQLITE_OUTBOX_RENEW_LEASE: &str = include_str!("../sql/sqlite/outbox_renew_lease.sql");
/// Mark a message as published.
pub const SQLITE_OUTBOX_MARK_PUBLISHED: &str =
    include_str!("../sql/sqlite/outbox_mark_published.sql");
/// Select the retry count of a message.
pub const SQLITE_OUTBOX_SELECT_RETRY_COUNT: &str =
    include_str!("../sql/sqlite/outbox_select_retry_count.sql");
/// Mark a message as failed.
pub const SQLITE_OUTBOX_MARK_FAILED: &str = include_str!("../sql/sqlite/outbox_mark_failed.sql");
/// Count pending messages.
pub const SQLITE_OUTBOX_PENDING_COUNT: &str =
    include_str!("../sql/sqlite/outbox_pending_count.sql");
/// Count published messages.
pub const SQLITE_OUTBOX_PUBLISHED_COUNT: &str =
    include_str!("../sql/sqlite/outbox_published_count.sql");
/// Count failed messages.
pub const SQLITE_OUTBOX_FAILED_COUNT: &str = include_str!("../sql/sqlite/outbox_failed_count.sql");
/// Fetch failed messages.
pub const SQLITE_OUTBOX_FETCH_FAILED: &str = include_str!("../sql/sqlite/outbox_fetch_failed.sql");
/// Re-process a failed message.
pub const SQLITE_OUTBOX_REPROCESS_FAILED: &str =
    include_str!("../sql/sqlite/outbox_reprocess_failed.sql");
/// Purge published messages.
pub const SQLITE_OUTBOX_PURGE_PUBLISHED: &str =
    include_str!("../sql/sqlite/outbox_purge_published.sql");
