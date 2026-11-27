UPDATE rume_api_keys
SET revoked_at = ?
WHERE id = ? AND revoked_at IS NULL;
