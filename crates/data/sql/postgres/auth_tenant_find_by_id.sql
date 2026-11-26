SELECT id, slug, metadata_json
FROM rume_tenants
WHERE id = $1 AND active = TRUE;
