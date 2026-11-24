INSERT INTO permissions (id, action, resource) VALUES
	(29, 'create', 'env_var'),
	(30, 'read', 'env_var'),
	(31, 'update', 'env_var'),
	(32, 'delete', 'env_var')
ON CONFLICT DO NOTHING;

INSERT INTO role_permissions (role_id, permission_id)
SELECT 1, id FROM permissions WHERE resource = 'env_var'
ON CONFLICT DO NOTHING;

INSERT INTO role_permissions (role_id, permission_id)
SELECT 2, id FROM permissions WHERE resource = 'env_var' AND action = 'read'
ON CONFLICT DO NOTHING;
