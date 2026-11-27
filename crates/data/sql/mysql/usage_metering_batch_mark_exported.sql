UPDATE rume_usage_export_batches
SET exported_at = CURRENT_TIMESTAMP(6),
    claim_token = NULL,
    claimed_until = NULL
WHERE batch_id = ?;
