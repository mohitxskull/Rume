CREATE TABLE IF NOT EXISTS rume_sagas (
    saga_id VARCHAR(128) PRIMARY KEY,
    saga_name VARCHAR(255) NOT NULL,
    status_json LONGTEXT NOT NULL,
    completed_steps_json LONGTEXT NOT NULL,
    compensated_steps_json LONGTEXT NOT NULL,
    is_terminal TINYINT(1) NOT NULL DEFAULT 0,
    updated_at BIGINT NOT NULL,
    version BIGINT NOT NULL DEFAULT 1,
    claim_token VARCHAR(128),
    claim_expires_at BIGINT,
    INDEX idx_rume_sagas_incomplete (is_terminal, updated_at)
) ENGINE=InnoDB;
