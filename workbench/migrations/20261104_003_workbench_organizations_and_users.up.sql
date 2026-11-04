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
