UPDATE rume_usage_export_batches
SET claimed_until = FLOOR(UNIX_TIMESTAMP(NOW(3)) * 1000) + ?
WHERE batch_id = ?;
