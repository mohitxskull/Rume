-- Schema DDL for rume_webhook_deliveries in SQLite.
CREATE TABLE IF NOT EXISTS rume_webhook_deliveries (
    id              TEXT    NOT NULL PRIMARY KEY,
    endpoint_id     TEXT    NOT NULL,
    event_type      TEXT    NOT NULL,
    payload         TEXT    NOT NULL,
    status          TEXT    NOT NULL DEFAULT 'pending',
    attempt         INTEGER NOT NULL DEFAULT 0,
    next_attempt_at TEXT,
    last_response   TEXT,
    created_at      TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at      TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);
CREATE INDEX IF NOT EXISTS rume_webhook_deliveries_status
    ON rume_webhook_deliveries (status, next_attempt_at);
