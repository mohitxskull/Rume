//! Database migration definitions, runner, and preflight verification.

mod preflight;
mod runner;

pub use preflight::{CREATE_MIGRATIONS_TABLE_DDL, migrations_schema_ddl};
pub use runner::{
    Migration, MigrationSummary, Migrator, RUME_AUDIT_MIGRATION_ID, RUME_AUTH_MIGRATION_ID,
    RUME_CORE_MIGRATION_ID, RUME_JOBS_QUEUE_PRIORITY_MIGRATION_ID,
};

#[cfg(test)]
mod tests;
