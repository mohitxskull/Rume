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
    then_dispatched TINYINT(1) NOT NULL DEFAULT 0,
    finally_dispatched TINYINT(1) NOT NULL DEFAULT 0,
    created_at BIGINT NOT NULL,
    finished_at BIGINT
) ENGINE=InnoDB;

CREATE TABLE IF NOT EXISTS rume_batch_job_outcomes (
    batch_id VARCHAR(64) NOT NULL,
    job_id VARCHAR(128) NOT NULL,
    outcome VARCHAR(16) NOT NULL,
    PRIMARY KEY (batch_id, job_id)
) ENGINE=InnoDB;
