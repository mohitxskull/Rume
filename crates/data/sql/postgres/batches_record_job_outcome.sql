INSERT INTO rume_batch_job_outcomes (batch_id, job_id, outcome)
VALUES ($1, $2, $3)
ON CONFLICT (batch_id, job_id) DO NOTHING;
