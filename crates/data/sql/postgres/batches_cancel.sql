UPDATE rume_job_batches
SET status = 'cancelled',
    finished_at = COALESCE(finished_at, FLOOR(EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT)
WHERE id = $1
  AND status = 'running';
