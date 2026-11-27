INSERT INTO rume_usage_export_batches (batch_id, payload_json, claimed_until)
VALUES (?1, ?2, CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER) + ?3);
