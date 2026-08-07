INSERT INTO permissions (id, action, resource) VALUES
	(33, 'create', 'domain'),
	(34, 'read', 'domain'),
	(35, 'update', 'domain'),
	(36, 'delete', 'domain')
ON CONFLICT DO NOTHING;

INSERT INTO role_permissions (role_id, permission_id)
SELECT 1, id FROM permissions WHERE resource = 'domain'
ON CONFLICT DO NOTHING;

INSERT INTO role_permissions (role_id, permission_id)
SELECT 2, id FROM permissions WHERE resource = 'domain' AND action = 'read'
ON CONFLICT DO NOTHING;
