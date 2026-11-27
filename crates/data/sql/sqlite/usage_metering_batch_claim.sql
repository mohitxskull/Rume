UPDATE rume_usage_export_batches
SET claimed_until = CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER) + ?1
WHERE batch_id = ?2;
