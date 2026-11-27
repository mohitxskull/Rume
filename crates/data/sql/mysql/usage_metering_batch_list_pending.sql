SELECT batch_id, payload_json
FROM rume_usage_export_batches
WHERE exported_at IS NULL
  AND (claimed_until IS NULL OR claimed_until <= FLOOR(UNIX_TIMESTAMP(NOW(3)) * 1000))
ORDER BY created_at ASC, batch_id ASC
FOR UPDATE SKIP LOCKED;
