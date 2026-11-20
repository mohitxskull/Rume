UPDATE rume_outbox AS o
SET status = 'inflight',
    lease_token = v.token,
    lease_expires_at = FLOOR(EXTRACT(EPOCH FROM clock_timestamp()))::BIGINT + $3
FROM unnest($1::text[], $2::text[]) AS v(id, token)
WHERE o.id = v.id AND o.status = 'pending'
RETURNING o.id, o.lease_expires_at
