-- Schema DDL for rume_usage_counters in PostgreSQL.
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
