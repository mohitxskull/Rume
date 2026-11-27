UPDATE rume_usage_export_batches
SET claim_token = NULL,
    claimed_until = NULL
WHERE batch_id = ?
  AND claim_token = ?
  AND exported_at IS NULL;
