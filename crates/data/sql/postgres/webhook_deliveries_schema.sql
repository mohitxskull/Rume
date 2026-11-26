-- Schema DDL for rume_webhook_deliveries in PostgreSQL.
CREATE TABLE IF NOT EXISTS rume_webhook_deliveries (
    id              TEXT        NOT NULL PRIMARY KEY,
    endpoint_id     TEXT        NOT NULL,
    event_type      TEXT        NOT NULL,
    payload         JSONB       NOT NULL,
    status          TEXT        NOT NULL DEFAULT 'pending',
    attempt         INTEGER     NOT NULL DEFAULT 0,
    next_attempt_at TIMESTAMPTZ,
    last_response   TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS rume_webhook_deliveries_status
    ON rume_webhook_deliveries (status, next_attempt_at);
