UPDATE rume_usage_export_batches
SET claimed_until = (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT + $1
WHERE batch_id = $2;
