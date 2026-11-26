UPDATE rume_api_keys
SET last_used_at = $1
WHERE id = $2;
