-- Why: MySQL evaluates UPDATE SET assignments left-to-right, so `pending_jobs`
-- is already decremented when `status` and `finished_at` are evaluated.
UPDATE rume_job_batches
SET completed_jobs = completed_jobs + 1,
    pending_jobs = GREATEST(pending_jobs - 1, 0),
    status = CASE
        WHEN status = 'cancelled' THEN 'cancelled'
        WHEN pending_jobs = 0 AND failed_jobs > 0 THEN 'failed'
        WHEN pending_jobs = 0 THEN 'completed'
        ELSE status
    END,
    finished_at = CASE
        WHEN pending_jobs = 0 AND finished_at IS NULL
        THEN FLOOR(UNIX_TIMESTAMP(NOW(3)) * 1000)
        ELSE finished_at
    END
WHERE id = ?;
