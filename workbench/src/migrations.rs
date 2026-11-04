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

    // Migration 3: Workbench Organizations & Persisted Users
    migrator = migrator.add_migration(
        Migration::from_sql(
            "20261104_003_workbench_organizations_and_users",
            "Create Workbench organizations and users tables with default seeds",
            r#"
            CREATE TABLE IF NOT EXISTS workbench_organizations (
                id VARCHAR(64) PRIMARY KEY,
                name VARCHAR(255) NOT NULL,
                created_at BIGINT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS workbench_users (
                id VARCHAR(64) PRIMARY KEY,
                organization_id VARCHAR(64) NOT NULL REFERENCES workbench_organizations(id) ON DELETE CASCADE,
                email VARCHAR(255) NOT NULL UNIQUE,
                name VARCHAR(255) NOT NULL,
                role VARCHAR(32) NOT NULL,
                api_token VARCHAR(128) NOT NULL UNIQUE,
                created_at BIGINT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_workbench_users_org ON workbench_users(organization_id);
            CREATE INDEX IF NOT EXISTS idx_workbench_users_token ON workbench_users(api_token);

            -- Default bootstrap seed organizations
            INSERT INTO workbench_organizations (id, name, created_at)
            VALUES
                ('acme-corp', 'Acme Corporation', 1700000000000),
                ('globex', 'Globex Corporation', 1700000000000)
            ON CONFLICT (id) DO NOTHING;

            -- Default bootstrap seed users with pre-configured API tokens
            INSERT INTO workbench_users (id, organization_id, email, name, role, api_token, created_at)
            VALUES
                ('user-acme-admin', 'acme-corp', 'admin@acme.com', 'Acme Admin', 'admin', 'admin-token', 1700000000000),
                ('user-acme-member', 'acme-corp', 'member@acme.com', 'Acme Member', 'member', 'member-token', 1700000000000),
                ('user-acme-viewer', 'acme-corp', 'viewer@acme.com', 'Acme Viewer', 'viewer', 'viewer-token', 1700000000000),
                ('user-globex-admin', 'globex', 'admin@globex.com', 'Globex Admin', 'admin', 'globex-admin-token', 1700000000000),
                ('user-globex-member', 'globex', 'member@globex.com', 'Globex Member', 'member', 'globex-member-token', 1700000000000)
            ON CONFLICT (id) DO NOTHING;
            "#,
        )
        .with_down(
            r#"
            DROP TABLE IF EXISTS workbench_users;
            DROP TABLE IF EXISTS workbench_organizations;
            "#,
        ),
    );

    migrator
}
