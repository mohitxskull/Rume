DELETE FROM rume_sessions WHERE expires_at <= $1;
