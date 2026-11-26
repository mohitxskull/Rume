SELECT id, slug, metadata_json
FROM rume_tenants
WHERE slug = $1 AND active = TRUE;
