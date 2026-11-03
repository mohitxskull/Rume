UPDATE rume_outbox
SET status = 'pending',
    lease_token = NULL,
    lease_expires_at = NULL
WHERE status = 'inflight'
  AND (lease_expires_at <= $1 OR lease_expires_at IS NULL);
