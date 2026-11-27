UPDATE rume_job_batches
SET completed_jobs = completed_jobs + 1,
    pending_jobs = GREATEST(pending_jobs - 1, 0),
    status = CASE
        WHEN status = 'cancelled' THEN 'cancelled'
        WHEN GREATEST(pending_jobs - 1, 0) = 0 AND failed_jobs > 0 THEN 'failed'
        WHEN GREATEST(pending_jobs - 1, 0) = 0 THEN 'completed'
        ELSE status
    END,
    finished_at = CASE
        WHEN GREATEST(pending_jobs - 1, 0) = 0 AND finished_at IS NULL
        THEN FLOOR(EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT
        ELSE finished_at
    END
WHERE id = $1
RETURNING id, name, total_jobs, pending_jobs, completed_jobs, failed_jobs, status,
          then_job_name, then_job_payload, finally_job_name, finally_job_payload,
          callbacks_dispatched, created_at, finished_at;
