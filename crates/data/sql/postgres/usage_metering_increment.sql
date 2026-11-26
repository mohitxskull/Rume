INSERT INTO rume_usage_counters (id, tenant_id, metric, window, count, period_start)
VALUES ($1, $2, $3, $4, $5, $6::timestamptz)
ON CONFLICT (id) DO UPDATE SET
    count = rume_usage_counters.count + EXCLUDED.count,
    updated_at = NOW();
