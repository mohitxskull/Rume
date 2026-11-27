INSERT INTO rume_usage_export_batches (batch_id, payload_json, claim_token, claimed_until)
VALUES (?, ?, ?, FLOOR(UNIX_TIMESTAMP(NOW(3)) * 1000) + ?);
