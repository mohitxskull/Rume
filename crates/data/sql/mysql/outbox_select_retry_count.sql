SELECT retry_count FROM rume_outbox WHERE id = ? AND lease_token = ? AND status = 'inflight' AND lease_expires_at > UNIX_TIMESTAMP()
