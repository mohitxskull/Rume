SELECT role, candidate_id, term FROM rume_leaders WHERE role = $1 AND expires_at > (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT
