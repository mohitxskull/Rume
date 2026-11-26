INSERT INTO rume_api_keys (
    id, prefix, key_hash, user_id, tenant_id, name, scopes_json, expires_at, revoked_at, created_at, last_used_at
)
VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11);
