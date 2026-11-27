UPDATE rume_job_batches
SET then_dispatched = FALSE
WHERE id = $1;
