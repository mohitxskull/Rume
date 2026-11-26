CREATE TABLE IF NOT EXISTS rume_sessions (
    id VARCHAR(128) PRIMARY KEY,
    user_id VARCHAR(255) NOT NULL,
    tenant_id VARCHAR(128),
    roles_json TEXT NOT NULL,
    permissions_json TEXT NOT NULL,
    data_json TEXT NOT NULL,
    expires_at BIGINT NOT NULL,
    created_at BIGINT NOT NULL,
    INDEX idx_rume_sessions_expires_at (expires_at),
    INDEX idx_rume_sessions_user_id (user_id)
) ENGINE=InnoDB;

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
    last_used_at BIGINT,
    INDEX idx_rume_api_keys_prefix (prefix),
    INDEX idx_rume_api_keys_user_id (user_id)
) ENGINE=InnoDB;

CREATE TABLE IF NOT EXISTS rume_roles (
    id VARCHAR(128) PRIMARY KEY,
    tenant_id VARCHAR(128) NOT NULL DEFAULT '',
    name VARCHAR(128) NOT NULL,
    description TEXT NOT NULL,
    UNIQUE KEY uq_rume_roles_tenant_name (tenant_id, name)
) ENGINE=InnoDB;

CREATE TABLE IF NOT EXISTS rume_role_permissions (
    role_id VARCHAR(128) NOT NULL,
    permission VARCHAR(255) NOT NULL,
    PRIMARY KEY (role_id, permission),
    CONSTRAINT fk_rume_role_permissions_role FOREIGN KEY (role_id) REFERENCES rume_roles(id) ON DELETE CASCADE
) ENGINE=InnoDB;

CREATE TABLE IF NOT EXISTS rume_user_roles (
    user_id VARCHAR(255) NOT NULL,
    tenant_id VARCHAR(128) NOT NULL DEFAULT '',
    role_id VARCHAR(128) NOT NULL,
    PRIMARY KEY (user_id, tenant_id, role_id),
    INDEX idx_rume_user_roles_lookup (user_id, tenant_id),
    CONSTRAINT fk_rume_user_roles_role FOREIGN KEY (role_id) REFERENCES rume_roles(id) ON DELETE CASCADE
) ENGINE=InnoDB;

CREATE TABLE IF NOT EXISTS rume_tenants (
    id VARCHAR(128) PRIMARY KEY,
    slug VARCHAR(128) NOT NULL,
    metadata_json TEXT NOT NULL,
    active TINYINT(1) NOT NULL DEFAULT 1,
    created_at BIGINT NOT NULL,
    UNIQUE KEY uq_rume_tenants_slug (slug)
) ENGINE=InnoDB;
