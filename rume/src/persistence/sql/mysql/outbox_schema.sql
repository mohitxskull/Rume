CREATE TABLE IF NOT EXISTS rume_outbox (
    id VARCHAR(64) PRIMARY KEY,
    event_type VARCHAR(255) NOT NULL,
    payload LONGBLOB NOT NULL,
    status VARCHAR(32) NOT NULL DEFAULT 'pending',
    retry_count INT NOT NULL DEFAULT 0,
    created_at BIGINT UNSIGNED NOT NULL,
    last_error TEXT,
    next_retry_at BIGINT UNSIGNED,
    lease_token VARCHAR(64),
    lease_expires_at BIGINT UNSIGNED,
    INDEX idx_rume_outbox_claim (status, created_at),
    INDEX idx_rume_outbox_lease (status, lease_expires_at)
) ENGINE=InnoDB;
