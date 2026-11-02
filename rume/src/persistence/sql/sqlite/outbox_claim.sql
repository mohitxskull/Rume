SELECT id, event_type, payload, status, retry_count,
       created_at, last_error, next_retry_at, lease_token, lease_expires_at
FROM rume_outbox
WHERE status = 'pending'
  AND (next_retry_at IS NULL OR next_retry_at <= ?1)
ORDER BY created_at ASC
LIMIT ?2;
