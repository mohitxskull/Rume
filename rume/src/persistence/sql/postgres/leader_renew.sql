UPDATE rume_leaders
SET expires_at = $1
WHERE role = $2 AND candidate_id = $3 AND expires_at > $4
RETURNING term;
