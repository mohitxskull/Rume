CREATE TABLE IF NOT EXISTS rume_sessions (
    id VARCHAR(128) PRIMARY KEY,
    user_id VARCHAR(255) NOT NULL,
    tenant_id VARCHAR(128),
    roles_json TEXT NOT NULL,
    permissions_json TEXT NOT NULL,
    data_json TEXT NOT NULL,
    expires_at BIGINT NOT NULL,
    created_at BIGINT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_rume_sessions_expires_at
ON rume_sessions (expires_at);

CREATE INDEX IF NOT EXISTS idx_rume_sessions_user_id
ON rume_sessions (user_id);

CREATE TABLE IF NOT EXISTS rume_api_keys (
    id VARCHAR(128) PRIMARY KEY,
    prefix VARCHAR(32) NOT NULL,
    key_hash VARCHAR(64) NOT NULL,
    user_id VARCHAR(255) NOT NULL,
    tenant_id VARCHAR(128),
    name VARCHAR(255) NOT NULL,
    scopes_json TEXT NOT NULL,
    expires_at BIGINT,
    revoked_at BIGINT,
    created_at BIGINT NOT NULL,
    last_used_at BIGINT
);

CREATE INDEX IF NOT EXISTS idx_rume_api_keys_prefix
ON rume_api_keys (prefix);

CREATE INDEX IF NOT EXISTS idx_rume_api_keys_user_id
ON rume_api_keys (user_id);

CREATE TABLE IF NOT EXISTS rume_roles (
    id VARCHAR(128) PRIMARY KEY,
    tenant_id VARCHAR(128) NOT NULL DEFAULT '',
    name VARCHAR(128) NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    UNIQUE (tenant_id, name)
);

CREATE TABLE IF NOT EXISTS rume_role_permissions (
    role_id VARCHAR(128) NOT NULL,
    permission VARCHAR(255) NOT NULL,
    PRIMARY KEY (role_id, permission),
    FOREIGN KEY (role_id) REFERENCES rume_roles(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS rume_user_roles (
    user_id VARCHAR(255) NOT NULL,
    tenant_id VARCHAR(128) NOT NULL DEFAULT '',
    role_id VARCHAR(128) NOT NULL,
    PRIMARY KEY (user_id, tenant_id, role_id),
    FOREIGN KEY (role_id) REFERENCES rume_roles(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_rume_user_roles_lookup
ON rume_user_roles (user_id, tenant_id);

CREATE TABLE IF NOT EXISTS rume_tenants (
    id VARCHAR(128) PRIMARY KEY,
    slug VARCHAR(128) NOT NULL UNIQUE,
    metadata_json TEXT NOT NULL,
    active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at BIGINT NOT NULL
);
