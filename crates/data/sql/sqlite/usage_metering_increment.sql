INSERT INTO rume_usage_counters (id, tenant_id, metric, window, count, period_start)
VALUES (?, ?, ?, ?, ?, ?)
ON CONFLICT (id) DO UPDATE SET
    count = rume_usage_counters.count + EXCLUDED.count,
    updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now');
