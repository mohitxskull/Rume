DELETE FROM rume_outbox WHERE id IN (
    SELECT id FROM rume_outbox
    WHERE status = 'published'
      AND created_at <= FLOOR(EXTRACT(EPOCH FROM clock_timestamp()))::BIGINT - $1
    ORDER BY created_at ASC
    LIMIT $2
)
