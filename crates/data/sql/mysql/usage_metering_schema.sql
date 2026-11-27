-- Schema DDL for rume_usage_counters and rume_usage_export_batches in MySQL.
CREATE TABLE IF NOT EXISTS rume_usage_counters (
    id           VARCHAR(255) NOT NULL PRIMARY KEY,
    tenant_id    VARCHAR(128) NOT NULL,
    metric       VARCHAR(255) NOT NULL,
    `window`     VARCHAR(64)  NOT NULL,
    count        BIGINT       NOT NULL DEFAULT 0,
    period_start DATETIME(6)  NOT NULL,
    updated_at   DATETIME(6)  NOT NULL DEFAULT CURRENT_TIMESTAMP(6) ON UPDATE CURRENT_TIMESTAMP(6),
    UNIQUE KEY rume_usage_counters_tenant_metric_window (tenant_id, metric, `window`, period_start)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE IF NOT EXISTS rume_usage_export_batches (
    batch_id      VARCHAR(64) NOT NULL PRIMARY KEY,
    payload_json  LONGTEXT    NOT NULL,
    created_at    DATETIME(6) NOT NULL DEFAULT CURRENT_TIMESTAMP(6),
    claimed_until BIGINT      NULL,
    exported_at   DATETIME(6) NULL,
    KEY rume_usage_export_batches_pending (exported_at, claimed_until, created_at)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
