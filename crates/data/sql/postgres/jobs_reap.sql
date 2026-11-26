UPDATE rume_jobs
SET status = 'pending',
    assigned_worker = NULL,
    lease_expires_at = NULL,
    lease_token = NULL
WHERE id IN (
    SELECT id FROM rume_jobs
    WHERE status = 'in_flight'
      AND assigned_worker = ANY($1)
    FOR UPDATE SKIP LOCKED
);
