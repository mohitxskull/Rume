DELETE FROM rume_locks WHERE expires_at <= (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT
