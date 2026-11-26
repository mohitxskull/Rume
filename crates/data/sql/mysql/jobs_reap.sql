UPDATE rume_jobs
SET status = 'pending',
    assigned_worker = NULL,
    lease_expires_at = NULL,
    lease_token = NULL
WHERE status = 'in_flight'
  AND assigned_worker = ?;
