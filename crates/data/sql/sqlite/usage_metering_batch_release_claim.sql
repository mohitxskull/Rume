UPDATE rume_usage_export_batches
SET claim_token = NULL,
    claimed_until = NULL
WHERE batch_id = ?1
  AND claim_token = ?2
  AND exported_at IS NULL;
