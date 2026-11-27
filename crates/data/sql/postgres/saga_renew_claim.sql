UPDATE rume_sagas
SET claim_expires_at = (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT + $3
WHERE saga_id = $1
  AND claim_token = $2
  AND claim_expires_at > (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT;
