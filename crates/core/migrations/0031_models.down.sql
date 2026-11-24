DELETE FROM role_permissions WHERE permission_id IN (SELECT id FROM permissions WHERE resource = 'model');
DELETE FROM permissions WHERE resource = 'model';
DROP TABLE IF EXISTS models;
