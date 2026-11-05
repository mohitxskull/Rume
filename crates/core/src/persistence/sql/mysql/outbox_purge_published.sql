DELETE FROM rume_outbox WHERE status = 'published' AND created_at <= UNIX_TIMESTAMP() - ? ORDER BY created_at ASC LIMIT ?
