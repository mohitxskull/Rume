INSERT INTO rume_usage_export_batches (batch_id, payload_json, claimed_until)
VALUES ($1, $2, (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT + $3);
