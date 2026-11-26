UPDATE rume_jobs
SET status = 'in_flight',
    assigned_worker = ?,
    attempt = attempt + 1,
    lease_expires_at = FLOOR(UNIX_TIMESTAMP(NOW(3)) * 1000) + ?,
    lease_token = ?
WHERE id = ?;
