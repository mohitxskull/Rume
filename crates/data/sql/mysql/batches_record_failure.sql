UPDATE rume_job_batches
SET failed_jobs = failed_jobs + 1,
    pending_jobs = GREATEST(pending_jobs - 1, 0),
    status = CASE
        WHEN status = 'cancelled' THEN 'cancelled'
        WHEN GREATEST(pending_jobs - 1, 0) = 0 THEN 'failed'
        ELSE status
    END,
    finished_at = CASE
        WHEN GREATEST(pending_jobs - 1, 0) = 0 AND finished_at IS NULL
        THEN FLOOR(UNIX_TIMESTAMP(NOW(3)) * 1000)
        ELSE finished_at
    END
WHERE id = ?;
