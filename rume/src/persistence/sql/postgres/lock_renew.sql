UPDATE rume_locks
SET expires_at = $1
WHERE resource = $2 AND lease_id = $3 AND expires_at > $4;
