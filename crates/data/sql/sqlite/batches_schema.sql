CREATE TABLE IF NOT EXISTS rume_job_batches (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    total_jobs INTEGER NOT NULL,
    pending_jobs INTEGER NOT NULL,
    completed_jobs INTEGER NOT NULL DEFAULT 0,
    failed_jobs INTEGER NOT NULL DEFAULT 0,
    status TEXT NOT NULL DEFAULT 'running',
    then_job_name TEXT,
    then_job_payload BLOB,
    finally_job_name TEXT,
    finally_job_payload BLOB,
    then_dispatched INTEGER NOT NULL DEFAULT 0,
    finally_dispatched INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    finished_at INTEGER
);

CREATE TABLE IF NOT EXISTS rume_batch_job_outcomes (
    batch_id TEXT NOT NULL,
    job_id TEXT NOT NULL,
    outcome TEXT NOT NULL,
    PRIMARY KEY (batch_id, job_id)
);
