UPDATE rume_sagas
SET claim_token = NULL,
    claim_expires_at = NULL
WHERE saga_id = ?
  AND claim_token = ?;
