UPDATE rume_outbox
SET status = 'pending',
    lease_token = NULL,
    lease_expires_at = NULL
WHERE status = 'inflight'
  AND (lease_expires_at <= FLOOR(EXTRACT(EPOCH FROM clock_timestamp()))::BIGINT OR lease_expires_at IS NULL);
