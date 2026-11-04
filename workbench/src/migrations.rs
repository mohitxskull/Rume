use rume::coordination::postgres::{PostgresDistributedLock, PostgresLeaderElector};
use rume::jobs::PostgresJobQueue;
use rume::persistence::migration::{Migration, Migrator};
use rume::persistence::postgres::OUTBOX_TABLE_SCHEMA;

/// Builds the database migrator for the Workbench reference application.
pub fn workbench_migrator() -> Migrator {
    let mut migrator = Migrator::new();

    // Migration 1: Rume Core Schemas (Outbox, Jobs, Locks, Leader)
    migrator = migrator.add_migration(
        Migration::from_sql(
            "20261104_001_rume_core_schemas",
            "Initialize Rume core persistence tables",
            format!(
                "{}\n{}\n{}\n{}",
                OUTBOX_TABLE_SCHEMA,
                PostgresJobQueue::SCHEMA,
                PostgresDistributedLock::SCHEMA,
                PostgresLeaderElector::SCHEMA
            ),
        )
        .with_down(
            r#"
            DROP TABLE IF EXISTS rume_leaders;
            DROP TABLE IF EXISTS rume_locks;
            DROP TABLE IF EXISTS rume_jobs;
            DROP TABLE IF EXISTS rume_outbox;
            "#,
        ),
    );

    // Migration 2: Workbench Domain Tables (Projects & Issues)
    migrator = migrator.add_migration(
        Migration::from_sql(
            "20261104_002_workbench_domain_schemas",
            "Create Workbench project and issue tables",
            r#"
            CREATE TABLE IF NOT EXISTS workbench_projects (
                id VARCHAR(64) PRIMARY KEY,
                tenant_id VARCHAR(64) NOT NULL,
                name VARCHAR(255) NOT NULL,
                description TEXT,
                created_at BIGINT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS workbench_issues (
                id VARCHAR(64) PRIMARY KEY,
                tenant_id VARCHAR(64) NOT NULL,
                project_id VARCHAR(64) NOT NULL REFERENCES workbench_projects(id) ON DELETE CASCADE,
                title VARCHAR(255) NOT NULL,
                description TEXT,
                status VARCHAR(32) NOT NULL,
                priority VARCHAR(32) NOT NULL,
                assignee_id VARCHAR(64),
                created_at BIGINT NOT NULL,
                updated_at BIGINT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_workbench_projects_tenant ON workbench_projects(tenant_id);
            CREATE INDEX IF NOT EXISTS idx_workbench_issues_tenant_project ON workbench_issues(tenant_id, project_id);
            "#,
        )
        .with_down(
            r#"
            DROP TABLE IF EXISTS workbench_issues;
            DROP TABLE IF EXISTS workbench_projects;
            "#,
        ),
    );

    migrator
}
