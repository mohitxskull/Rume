SELECT id, event_type, payload, status, retry_count,
       created_at, last_error, next_retry_at, lease_token, lease_expires_at
FROM rume_outbox
WHERE status = 'pending'
  AND (next_retry_at IS NULL OR next_retry_at <= FLOOR(EXTRACT(EPOCH FROM clock_timestamp()))::BIGINT)
ORDER BY created_at ASC, id ASC
LIMIT $1
FOR UPDATE SKIP LOCKED;
