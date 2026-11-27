SELECT batch_id, payload_json
FROM rume_usage_export_batches
WHERE exported_at IS NULL
  AND (claimed_until IS NULL OR claimed_until <= (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT)
ORDER BY created_at ASC, batch_id ASC
FOR UPDATE SKIP LOCKED;
