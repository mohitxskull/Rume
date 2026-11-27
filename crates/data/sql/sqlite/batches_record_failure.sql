UPDATE rume_job_batches
SET failed_jobs = failed_jobs + 1,
    pending_jobs = MAX(pending_jobs - 1, 0),
    status = CASE
        WHEN status = 'cancelled' THEN 'cancelled'
        WHEN MAX(pending_jobs - 1, 0) = 0 THEN 'failed'
        ELSE status
    END,
    finished_at = CASE
        WHEN MAX(pending_jobs - 1, 0) = 0 AND finished_at IS NULL
        THEN CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)
        ELSE finished_at
    END
WHERE id = ?1
RETURNING id, name, total_jobs, pending_jobs, completed_jobs, failed_jobs, status,
          then_job_name, then_job_payload, finally_job_name, finally_job_payload,
          callbacks_dispatched, created_at, finished_at;
