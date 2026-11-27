INSERT INTO rume_tenants (id, slug, metadata_json, active, created_at)
VALUES (?, ?, ?, 1, ?)
ON DUPLICATE KEY UPDATE
    slug = VALUES(slug),
    metadata_json = VALUES(metadata_json),
    active = 1;
