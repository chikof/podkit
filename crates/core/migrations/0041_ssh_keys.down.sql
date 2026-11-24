-- Restores the per-resource columns from the vault, then removes it. A server or application
-- keeps the key it uses; keys nothing used are lost.
ALTER TABLE servers ADD COLUMN ssh_private_key BYTEA;
ALTER TABLE applications ADD COLUMN deploy_key BYTEA;

UPDATE servers s SET ssh_private_key = k.private_key FROM ssh_keys k WHERE k.id = s.ssh_key_id;
UPDATE applications a SET deploy_key = k.private_key FROM ssh_keys k WHERE k.id = a.deploy_key_id;

ALTER TABLE servers DROP CONSTRAINT servers_local_no_ssh;
ALTER TABLE servers
	ADD CONSTRAINT servers_local_no_ssh CHECK (
		(is_local AND ssh_user IS NULL AND ssh_private_key IS NULL)
		OR
		(NOT is_local AND ssh_user IS NOT NULL AND ssh_private_key IS NOT NULL)
	);

DROP INDEX IF EXISTS idx_applications_deploy_key_id;
DROP INDEX IF EXISTS idx_servers_ssh_key_id;
ALTER TABLE servers DROP CONSTRAINT servers_ssh_key_fk;
ALTER TABLE servers DROP COLUMN ssh_key_id;
ALTER TABLE applications DROP COLUMN deploy_key_id;
DROP TABLE ssh_keys;

DELETE FROM role_permissions WHERE permission_id IN (SELECT id FROM permissions WHERE resource = 'ssh_key');
DELETE FROM permissions WHERE resource = 'ssh_key';
