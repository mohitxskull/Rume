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
