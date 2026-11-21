UPDATE rume_outbox SET status = 'inflight', lease_token = ?1, lease_expires_at = CAST(strftime('%s', 'now') AS INTEGER) + ?2 WHERE id = ?3 AND status = 'pending' RETURNING lease_expires_at
