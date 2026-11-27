UPDATE rume_usage_export_batches
SET claim_token = ?1,
    claimed_until = CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER) + ?2
WHERE batch_id = ?3;
