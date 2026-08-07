DELETE FROM role_permissions WHERE permission_id IN (SELECT id FROM permissions WHERE resource = 'env_var');
DELETE FROM permissions WHERE resource = 'env_var';
