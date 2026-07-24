INSERT INTO permissions (id, action, resource) VALUES
	(1, 'create', 'team'),
	(2, 'read', 'team'),
	(3, 'update', 'team'),
	(4, 'delete', 'team'),
	(5, 'create', 'project'),
	(6, 'read', 'project'),
	(7, 'update', 'project'),
	(8, 'delete', 'project'),
	(9, 'create', 'membership'),
	(10, 'read', 'membership'),
	(11, 'update', 'membership'),
	(12, 'delete', 'membership'),
	(13, 'create', 'role'),
	(14, 'read', 'role'),
	(15, 'update', 'role'),
	(16, 'delete', 'role')
ON CONFLICT DO NOTHING;

INSERT INTO roles (id, team_id, name, is_default) VALUES
	(1, NULL, 'Owner', FALSE),
	(2, NULL, 'Member', TRUE)
ON CONFLICT DO NOTHING;

INSERT INTO role_permissions (role_id, permission_id)
SELECT 1, id FROM permissions
ON CONFLICT DO NOTHING;

INSERT INTO role_permissions (role_id, permission_id)
SELECT 2, id FROM permissions WHERE action = 'read'
ON CONFLICT DO NOTHING;
