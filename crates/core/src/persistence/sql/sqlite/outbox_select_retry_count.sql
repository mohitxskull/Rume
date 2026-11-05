SELECT retry_count FROM rume_outbox WHERE id = ?1 AND lease_token = ?2 AND status = 'inflight' AND lease_expires_at > CAST(strftime('%s', 'now') AS INTEGER)
