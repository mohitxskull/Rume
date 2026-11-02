INSERT INTO rume_locks (resource, lease_id, fencing_token, expires_at)
VALUES ($1, $2, nextval('rume_lock_fencing_seq'), $3)
ON CONFLICT (resource) DO UPDATE
SET lease_id = EXCLUDED.lease_id,
    fencing_token = nextval('rume_lock_fencing_seq'),
    expires_at = EXCLUDED.expires_at
WHERE rume_locks.expires_at <= $4
RETURNING fencing_token;
