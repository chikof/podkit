DELETE FROM role_permissions WHERE permission_id IN (SELECT id FROM permissions WHERE resource = 'stack');
DELETE FROM permissions WHERE resource = 'stack';
DROP TABLE IF EXISTS stack_services;
DROP TABLE IF EXISTS stacks;
