UPDATE rume_jobs
SET status = 'pending',
    assigned_worker = NULL,
    lease_expires_at = NULL
WHERE status = 'in_flight'
  AND (assigned_worker = ANY($1) OR lease_expires_at <= $2);
