UPDATE rume_locks
SET expires_at = (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT + $1
WHERE resource = $2 AND lease_id = $3 AND expires_at > (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT;
