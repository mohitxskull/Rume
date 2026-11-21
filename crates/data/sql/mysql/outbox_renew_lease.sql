UPDATE rume_outbox SET lease_expires_at = UNIX_TIMESTAMP() + ? WHERE id = ? AND lease_token = ? AND status = 'inflight' AND lease_expires_at > UNIX_TIMESTAMP()
