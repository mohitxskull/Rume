-- Schema DDL for rume_usage_counters in SQLite.
CREATE TABLE IF NOT EXISTS rume_usage_counters (
    id          TEXT    NOT NULL PRIMARY KEY,
    tenant_id   TEXT    NOT NULL,
    metric      TEXT    NOT NULL,
    window      TEXT    NOT NULL,
    count       INTEGER NOT NULL DEFAULT 0,
    period_start TEXT   NOT NULL,
    updated_at  TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);
CREATE UNIQUE INDEX IF NOT EXISTS rume_usage_counters_tenant_metric_window
    ON rume_usage_counters (tenant_id, metric, window, period_start);
