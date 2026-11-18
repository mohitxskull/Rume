CREATE TABLE IF NOT EXISTS rume_audit_log (
    id VARCHAR(64) PRIMARY KEY,
    action VARCHAR(255) NOT NULL,
    resource VARCHAR(255) NOT NULL,
    status VARCHAR(32) NOT NULL,
    actor_id VARCHAR(255),
    target_id VARCHAR(255),
    ip_address VARCHAR(64),
    metadata TEXT,
    created_at BIGINT NOT NULL
);
