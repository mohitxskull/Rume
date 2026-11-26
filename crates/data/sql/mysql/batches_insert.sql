INSERT INTO rume_job_batches (
    id, name, total_jobs, pending_jobs, completed_jobs, failed_jobs, status,
    then_job_name, then_job_payload, finally_job_name, finally_job_payload,
    callbacks_dispatched, created_at, finished_at
) VALUES (
    ?, ?, ?, ?, 0, 0, 'running',
    ?, ?, ?, ?,
    0, FLOOR(UNIX_TIMESTAMP(NOW(3)) * 1000), NULL
);
