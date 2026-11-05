UPDATE rume_outbox SET status = 'published', lease_token = NULL, lease_expires_at = NULL WHERE id = ? AND lease_token = ? AND status = 'inflight' AND lease_expires_at > UNIX_TIMESTAMP()
