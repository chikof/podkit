DELETE FROM role_permissions WHERE permission_id IN (SELECT id FROM permissions WHERE resource = 'logs');
DELETE FROM permissions WHERE resource = 'logs';
DROP TABLE log_settings;
