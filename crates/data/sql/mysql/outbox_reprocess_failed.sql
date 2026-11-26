UPDATE rume_outbox SET status = 'pending', retry_count = 0, last_error = NULL, next_retry_at = NULL, lease_token = NULL, lease_expires_at = NULL WHERE id = ? AND status = 'failed'
