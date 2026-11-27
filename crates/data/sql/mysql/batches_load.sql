SELECT id, name, total_jobs, pending_jobs, completed_jobs, failed_jobs, status,
       then_job_name, then_job_payload, finally_job_name, finally_job_payload,
       then_dispatched, finally_dispatched, created_at, finished_at
FROM rume_job_batches
WHERE id = ?;
