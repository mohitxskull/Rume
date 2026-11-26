CREATE TABLE IF NOT EXISTS rume_sessions (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL,
    tenant_id TEXT,
    roles_json TEXT NOT NULL,
    permissions_json TEXT NOT NULL,
    data_json TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_rume_sessions_expires_at
ON rume_sessions (expires_at);

CREATE INDEX IF NOT EXISTS idx_rume_sessions_user_id
ON rume_sessions (user_id);

CREATE TABLE IF NOT EXISTS rume_api_keys (
    id TEXT PRIMARY KEY,
    prefix TEXT NOT NULL,
    key_hash TEXT NOT NULL,
    user_id TEXT NOT NULL,
    tenant_id TEXT,
    name TEXT NOT NULL,
    scopes_json TEXT NOT NULL,
    expires_at INTEGER,
    revoked_at INTEGER,
    created_at INTEGER NOT NULL,
    last_used_at INTEGER
);

CREATE INDEX IF NOT EXISTS idx_rume_api_keys_prefix
ON rume_api_keys (prefix);

CREATE INDEX IF NOT EXISTS idx_rume_api_keys_user_id
ON rume_api_keys (user_id);

CREATE TABLE IF NOT EXISTS rume_roles (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL DEFAULT '',
    name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    UNIQUE (tenant_id, name)
);

CREATE TABLE IF NOT EXISTS rume_role_permissions (
    role_id TEXT NOT NULL,
    permission TEXT NOT NULL,
    PRIMARY KEY (role_id, permission),
    FOREIGN KEY (role_id) REFERENCES rume_roles(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS rume_user_roles (
    user_id TEXT NOT NULL,
    tenant_id TEXT NOT NULL DEFAULT '',
    role_id TEXT NOT NULL,
    PRIMARY KEY (user_id, tenant_id, role_id),
    FOREIGN KEY (role_id) REFERENCES rume_roles(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_rume_user_roles_lookup
ON rume_user_roles (user_id, tenant_id);

CREATE TABLE IF NOT EXISTS rume_tenants (
    id TEXT PRIMARY KEY,
    slug TEXT NOT NULL UNIQUE,
    metadata_json TEXT NOT NULL,
    active INTEGER NOT NULL DEFAULT 1,
    created_at INTEGER NOT NULL
);
