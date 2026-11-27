SELECT batch_id, payload_json
FROM rume_usage_export_batches
WHERE exported_at IS NULL
  AND (claimed_until IS NULL OR claimed_until <= CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER))
ORDER BY created_at ASC, batch_id ASC;
