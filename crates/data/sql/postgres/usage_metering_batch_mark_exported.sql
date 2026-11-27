UPDATE rume_usage_export_batches
SET exported_at = NOW(),
    claim_token = NULL,
    claimed_until = NULL
WHERE batch_id = $1;
