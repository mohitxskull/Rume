UPDATE rume_outbox SET status = 'inflight', lease_token = $1, lease_expires_at = FLOOR(EXTRACT(EPOCH FROM clock_timestamp()))::BIGINT + $2 WHERE id = $3 RETURNING lease_expires_at
