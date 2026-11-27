INSERT INTO rume_job_batches (
    id, name, total_jobs, pending_jobs, completed_jobs, failed_jobs, status,
    then_job_name, then_job_payload, finally_job_name, finally_job_payload,
    callbacks_dispatched, created_at, finished_at
) VALUES (
    $1, $2, $3, $4, 0, 0, 'running',
    $5, $6, $7, $8,
    FALSE, FLOOR(EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT, NULL
);
