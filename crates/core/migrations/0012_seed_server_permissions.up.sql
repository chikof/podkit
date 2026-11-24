INSERT INTO permissions (id, action, resource) VALUES
	(17, 'create', 'server'),
	(18, 'read', 'server'),
	(19, 'update', 'server'),
	(20, 'delete', 'server')
ON CONFLICT DO NOTHING;

INSERT INTO role_permissions (role_id, permission_id)
SELECT 1, id FROM permissions WHERE resource = 'server'
ON CONFLICT DO NOTHING;

INSERT INTO role_permissions (role_id, permission_id)
SELECT 2, id FROM permissions WHERE resource = 'server' AND action = 'read'
ON CONFLICT DO NOTHING;
