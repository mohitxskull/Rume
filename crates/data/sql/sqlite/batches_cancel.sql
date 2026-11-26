UPDATE rume_job_batches
SET status = 'cancelled',
    finished_at = COALESCE(finished_at, CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER))
WHERE id = ?1
  AND status = 'running';
