//! Database migration definitions, runner, and preflight verification.

mod preflight;
mod runner;

pub use runner::{
    Migration, MigrationSummary, Migrator, RUME_AUDIT_MIGRATION_ID, RUME_CORE_MIGRATION_ID,
};

#[cfg(test)]
mod tests;
