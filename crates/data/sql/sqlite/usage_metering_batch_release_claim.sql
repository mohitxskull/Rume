UPDATE rume_usage_export_batches
SET claimed_until = NULL
WHERE batch_id = ?1;
