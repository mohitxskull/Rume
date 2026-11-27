UPDATE rume_usage_export_batches
SET exported_at = CURRENT_TIMESTAMP(6),
    claimed_until = NULL
WHERE batch_id = ?;
