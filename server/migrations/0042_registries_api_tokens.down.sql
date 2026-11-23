DROP TABLE IF EXISTS api_tokens;
DELETE FROM role_permissions WHERE permission_id IN (SELECT id FROM permissions WHERE resource = 'registry');
DELETE FROM permissions WHERE resource = 'registry';
ALTER TABLE stacks DROP COLUMN IF EXISTS registry_id;
ALTER TABLE applications DROP COLUMN IF EXISTS registry_id;
DROP TABLE IF EXISTS registries;
