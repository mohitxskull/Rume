INSERT INTO rume_usage_export_batches (batch_id, payload_json, claimed_until)
VALUES (?, ?, FLOOR(UNIX_TIMESTAMP(NOW(3)) * 1000) + ?);
