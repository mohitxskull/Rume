UPDATE rume_usage_export_batches
SET claim_token = $1,
    claimed_until = (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT + $2
WHERE batch_id = $3;
