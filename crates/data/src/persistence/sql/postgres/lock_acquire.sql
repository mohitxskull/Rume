INSERT INTO rume_locks (resource, lease_id, fencing_token, expires_at)
VALUES ($1, $2, nextval('rume_lock_fencing_seq'), (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT + $3)
ON CONFLICT (resource) DO UPDATE
SET lease_id = EXCLUDED.lease_id,
    fencing_token = EXCLUDED.fencing_token,
    expires_at = (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT + $3
WHERE rume_locks.expires_at <= (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT
RETURNING fencing_token;
