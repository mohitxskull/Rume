UPDATE rume_leaders
SET expires_at = (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT + $1
WHERE role = $2 AND candidate_id = $3 AND expires_at > (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT
RETURNING term;
