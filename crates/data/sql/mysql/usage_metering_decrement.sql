UPDATE rume_usage_counters
SET count = CASE WHEN count >= ? THEN count - ? ELSE 0 END,
    updated_at = CURRENT_TIMESTAMP(6)
WHERE id = ?;
