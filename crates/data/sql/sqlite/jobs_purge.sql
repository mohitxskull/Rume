UPDATE rume_jobs
SET status = 'purged'
WHERE status = 'completed'
  AND completed_at <= CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER) - ?1;
