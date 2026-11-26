SELECT id, prefix, key_hash, user_id, tenant_id, name, scopes_json, expires_at, revoked_at, created_at, last_used_at
FROM rume_api_keys
WHERE user_id = ?
ORDER BY created_at DESC;
