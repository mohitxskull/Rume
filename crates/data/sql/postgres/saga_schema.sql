CREATE TABLE IF NOT EXISTS rume_sagas (
    saga_id TEXT PRIMARY KEY,
    saga_name TEXT NOT NULL,
    status_json TEXT NOT NULL,
    completed_steps_json TEXT NOT NULL,
    compensated_steps_json TEXT NOT NULL,
    is_terminal BOOLEAN NOT NULL DEFAULT FALSE,
    updated_at BIGINT NOT NULL,
    version BIGINT NOT NULL DEFAULT 1,
    claim_token TEXT,
    claim_expires_at BIGINT
);

CREATE INDEX IF NOT EXISTS idx_rume_sagas_incomplete ON rume_sagas (is_terminal, updated_at);
