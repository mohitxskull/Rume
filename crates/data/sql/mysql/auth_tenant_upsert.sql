INSERT INTO rume_tenants (id, slug, metadata_json, active, created_at)
VALUES (?, ?, ?, 1, ?) AS new
ON DUPLICATE KEY UPDATE
    slug = new.slug,
    metadata_json = new.metadata_json,
    active = 1;
