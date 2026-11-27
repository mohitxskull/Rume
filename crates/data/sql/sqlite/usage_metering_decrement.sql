UPDATE rume_usage_counters
SET count = CASE WHEN count >= ? THEN count - ? ELSE 0 END,
    updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
WHERE id = ?;
