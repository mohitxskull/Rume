SELECT id, event_type, payload, status, retry_count, created_at, last_error, next_retry_at, lease_token, lease_expires_at FROM rume_outbox WHERE status = 'failed' ORDER BY created_at ASC LIMIT ?1
