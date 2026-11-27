-- Schema DDL for rume_usage_counters and rume_usage_export_batches in PostgreSQL.
CREATE TABLE IF NOT EXISTS rume_usage_counters (
    id           TEXT        NOT NULL PRIMARY KEY,
    tenant_id    TEXT        NOT NULL,
    metric       TEXT        NOT NULL,
    "window"     TEXT        NOT NULL,
    count        BIGINT      NOT NULL DEFAULT 0,
    period_start TIMESTAMPTZ NOT NULL,
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE UNIQUE INDEX IF NOT EXISTS rume_usage_counters_tenant_metric_window
    ON rume_usage_counters (tenant_id, metric, "window", period_start);

CREATE TABLE IF NOT EXISTS rume_usage_export_batches (
    batch_id      TEXT        NOT NULL PRIMARY KEY,
    payload_json  TEXT        NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    claim_token   TEXT        NULL,
    claimed_until BIGINT      NULL,
    exported_at   TIMESTAMPTZ NULL
);
CREATE INDEX IF NOT EXISTS rume_usage_export_batches_pending
    ON rume_usage_export_batches (exported_at, claimed_until, created_at);
