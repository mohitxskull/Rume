UPDATE rume_usage_export_batches
SET exported_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'),
    claimed_until = NULL
WHERE batch_id = ?1;
