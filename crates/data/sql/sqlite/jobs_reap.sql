UPDATE rume_jobs
SET status = CASE
        WHEN attempt >= max_attempts THEN 'dead_letter'
        ELSE 'pending'
    END,
    dead_letter_reason = CASE
        WHEN attempt >= max_attempts THEN 'max attempts exceeded (worker abandoned)'
        ELSE dead_letter_reason
    END,
    dead_lettered_at = CASE
        WHEN attempt >= max_attempts THEN CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)
        ELSE dead_lettered_at
    END,
    assigned_worker = NULL,
    lease_expires_at = NULL,
    lease_token = NULL
WHERE status = 'in_flight'
  AND assigned_worker = ?1;
