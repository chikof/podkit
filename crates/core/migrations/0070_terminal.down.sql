DROP TABLE IF EXISTS terminal_sessions;
ALTER TABLE teams DROP COLUMN IF EXISTS terminal_record;
DELETE FROM role_permissions WHERE permission_id IN (SELECT id FROM permissions WHERE resource = 'terminal');
DELETE FROM permissions WHERE resource = 'terminal';
