SELECT COUNT(*) FROM rume_outbox WHERE status = 'pending' OR status = 'inflight'
