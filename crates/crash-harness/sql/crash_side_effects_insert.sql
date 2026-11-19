INSERT INTO crash_test_side_effects (job_id, executed_by, executed_at) VALUES ($1, $2, (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT)
