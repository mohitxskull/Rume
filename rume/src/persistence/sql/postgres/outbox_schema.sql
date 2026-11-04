CREATE TABLE IF NOT EXISTS rume_outbox (
    id VARCHAR(64) PRIMARY KEY,
    event_type VARCHAR(255) NOT NULL,
    payload BYTEA NOT NULL,
    status VARCHAR(32) NOT NULL DEFAULT 'pending',
    retry_count INT NOT NULL DEFAULT 0,
    created_at BIGINT NOT NULL,
    last_error TEXT,
    next_retry_at BIGINT,
    lease_token VARCHAR(64),
    lease_expires_at BIGINT,
    trace_context TEXT
);

CREATE INDEX IF NOT EXISTS idx_rume_outbox_pending
ON rume_outbox (status, created_at)
WHERE status = 'pending';

CREATE INDEX IF NOT EXISTS idx_rume_outbox_lease
ON rume_outbox (status, lease_expires_at)
WHERE status = 'inflight';

