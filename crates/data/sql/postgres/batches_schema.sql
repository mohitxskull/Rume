CREATE TABLE IF NOT EXISTS rume_job_batches (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    total_jobs INT NOT NULL,
    pending_jobs INT NOT NULL,
    completed_jobs INT NOT NULL DEFAULT 0,
    failed_jobs INT NOT NULL DEFAULT 0,
    status TEXT NOT NULL DEFAULT 'running',
    then_job_name TEXT,
    then_job_payload BYTEA,
    finally_job_name TEXT,
    finally_job_payload BYTEA,
    callbacks_dispatched BOOLEAN NOT NULL DEFAULT FALSE,
    created_at BIGINT NOT NULL,
    finished_at BIGINT
);
