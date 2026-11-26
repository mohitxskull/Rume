UPDATE rume_usage_counters
SET count = CASE WHEN count >= $1 THEN count - $2 ELSE 0 END,
    updated_at = NOW()
WHERE id = $3;
