DELETE FROM role_permissions WHERE permission_id IN (SELECT id FROM permissions WHERE resource = 'git_provider');
DELETE FROM permissions WHERE resource = 'git_provider';
DROP TABLE git_webhook_deliveries;
ALTER TABLE applications
	DROP CONSTRAINT applications_git_provider_repo,
	DROP COLUMN git_hook_id,
	DROP COLUMN git_repo,
	DROP COLUMN git_provider_id;
DROP TABLE git_providers;
