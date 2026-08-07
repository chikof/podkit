INSERT INTO permissions (id, action, resource) VALUES
	(21, 'create', 'application'),
	(22, 'read', 'application'),
	(23, 'update', 'application'),
	(24, 'delete', 'application')
ON CONFLICT DO NOTHING;

INSERT INTO role_permissions (role_id, permission_id)
SELECT 1, id FROM permissions WHERE resource = 'application'
ON CONFLICT DO NOTHING;

INSERT INTO role_permissions (role_id, permission_id)
SELECT 2, id FROM permissions WHERE resource = 'application' AND action = 'read'
ON CONFLICT DO NOTHING;
