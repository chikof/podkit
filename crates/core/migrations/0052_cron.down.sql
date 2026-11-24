DELETE FROM role_permissions
	WHERE permission_id IN (SELECT id FROM permissions WHERE resource = 'cron');
DELETE FROM permissions WHERE resource = 'cron';
DROP TABLE cron_runs;
DROP TABLE cron_tasks;
