ALTER TABLE rume_jobs ADD COLUMN IF NOT EXISTS queue TEXT NOT NULL DEFAULT 'default';
ALTER TABLE rume_jobs ADD COLUMN IF NOT EXISTS priority INT NOT NULL DEFAULT 0;
CREATE INDEX IF NOT EXISTS idx_rume_jobs_pending_queue_prio ON rume_jobs (queue, priority DESC, available_at ASC, created_at ASC) WHERE status = 'pending';
