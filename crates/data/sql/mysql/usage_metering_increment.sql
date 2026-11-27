INSERT INTO rume_usage_counters (id, tenant_id, metric, `window`, count, period_start)
VALUES (?, ?, ?, ?, ?, ?)
ON DUPLICATE KEY UPDATE
    count = count + VALUES(count),
    updated_at = CURRENT_TIMESTAMP(6);
