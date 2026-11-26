UPDATE rume_api_keys
SET revoked_at = $1
WHERE id = $2 AND revoked_at IS NULL;
