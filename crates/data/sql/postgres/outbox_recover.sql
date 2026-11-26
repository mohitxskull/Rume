UPDATE rume_outbox
SET retry_count = retry_count + 1,
    status = CASE
        WHEN retry_count + 1 >= $1 THEN 'failed'
        ELSE 'pending'
    END,
    last_error = CASE
        WHEN retry_count + 1 >= $1 THEN COALESCE(last_error, 'lease expired: max retries exceeded')
        ELSE last_error
    END,
    lease_token = NULL,
    lease_expires_at = NULL
WHERE status = 'inflight'
  AND (lease_expires_at <= FLOOR(EXTRACT(EPOCH FROM clock_timestamp()))::BIGINT OR lease_expires_at IS NULL);
