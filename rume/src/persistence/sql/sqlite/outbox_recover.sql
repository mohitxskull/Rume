UPDATE rume_outbox
SET status = 'pending',
    lease_token = NULL,
    lease_expires_at = NULL
WHERE status = 'inflight'
  AND (lease_expires_at <= CAST(strftime('%s', 'now') AS INTEGER) OR lease_expires_at IS NULL);
