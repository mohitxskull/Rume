UPDATE rume_outbox
SET last_error = CASE
        WHEN retry_count + 1 >= ? THEN COALESCE(last_error, 'lease expired: max retries exceeded')
        ELSE last_error
    END,
    status = CASE
        WHEN retry_count + 1 >= ? THEN 'failed'
        ELSE 'pending'
    END,
    retry_count = retry_count + 1,
    lease_token = NULL,
    lease_expires_at = NULL
WHERE status = 'inflight'
  AND (lease_expires_at <= UNIX_TIMESTAMP() OR lease_expires_at IS NULL);
