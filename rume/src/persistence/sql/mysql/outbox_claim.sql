SELECT id, event_type, payload, status, retry_count,
       created_at, last_error, next_retry_at, lease_token, lease_expires_at
FROM rume_outbox FORCE INDEX (idx_rume_outbox_claim)
WHERE status = 'pending'
  AND (next_retry_at IS NULL OR next_retry_at <= UNIX_TIMESTAMP())
ORDER BY created_at ASC, id ASC
LIMIT ?
FOR UPDATE SKIP LOCKED;
