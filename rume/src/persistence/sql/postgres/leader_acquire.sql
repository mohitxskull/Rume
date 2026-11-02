INSERT INTO rume_leaders (role, candidate_id, term, expires_at)
VALUES ($1, $2, nextval('rume_leader_term_seq'), $3)
ON CONFLICT (role) DO UPDATE
SET candidate_id = EXCLUDED.candidate_id,
    term = CASE
        WHEN rume_leaders.candidate_id = EXCLUDED.candidate_id THEN rume_leaders.term
        ELSE nextval('rume_leader_term_seq')
    END,
    expires_at = EXCLUDED.expires_at
WHERE rume_leaders.expires_at <= $4 OR rume_leaders.candidate_id = $2
RETURNING term;
