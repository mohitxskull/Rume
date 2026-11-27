CREATE TABLE IF NOT EXISTS rume_job_batches (
    id VARCHAR(64) PRIMARY KEY,
    name VARCHAR(255) NOT NULL,
    total_jobs INT NOT NULL,
    pending_jobs INT NOT NULL,
    completed_jobs INT NOT NULL DEFAULT 0,
    failed_jobs INT NOT NULL DEFAULT 0,
    status VARCHAR(32) NOT NULL DEFAULT 'running',
    then_job_name VARCHAR(255),
    then_job_payload LONGBLOB,
    finally_job_name VARCHAR(255),
    finally_job_payload LONGBLOB,
    callbacks_dispatched TINYINT(1) NOT NULL DEFAULT 0,
    created_at BIGINT NOT NULL,
    finished_at BIGINT
) ENGINE=InnoDB;
