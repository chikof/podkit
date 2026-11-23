DELETE FROM role_permissions WHERE permission_id IN (SELECT id FROM permissions WHERE resource = 'quota');
DELETE FROM permissions WHERE resource = 'quota';
DROP TABLE IF EXISTS billing_customers;
DROP TABLE IF EXISTS usage_records;
DROP TABLE IF EXISTS quota_levels;
DROP TABLE IF EXISTS team_quotas;
