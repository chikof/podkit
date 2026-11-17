INSERT INTO permissions (id, action, resource) VALUES
	(25, 'create', 'deployment'),
	(26, 'read', 'deployment'),
	(27, 'update', 'deployment'),
	(28, 'delete', 'deployment')
ON CONFLICT DO NOTHING;

INSERT INTO role_permissions (role_id, permission_id)
SELECT 1, id FROM permissions WHERE resource = 'deployment'
ON CONFLICT DO NOTHING;

INSERT INTO role_permissions (role_id, permission_id)
SELECT 2, id FROM permissions WHERE resource = 'deployment' AND action = 'read'
ON CONFLICT DO NOTHING;
