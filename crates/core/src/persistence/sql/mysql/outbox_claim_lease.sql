UPDATE rume_outbox SET status = 'inflight', lease_token = ?, lease_expires_at = UNIX_TIMESTAMP() + ? WHERE id = ?
