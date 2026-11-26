UPDATE rume_sagas
SET claim_token = $2,
    claim_expires_at = (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT + $3
WHERE saga_id = $1
  AND (claim_token IS NULL OR claim_expires_at <= (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT);
