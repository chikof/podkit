DELETE FROM role_permissions WHERE permission_id IN (SELECT id FROM permissions WHERE resource = 'audit');
DELETE FROM permissions WHERE resource = 'audit';
ALTER TABLE teams DROP COLUMN IF EXISTS audit_retention_days;
DROP TRIGGER IF EXISTS audit_events_append_only ON audit_events;
DROP FUNCTION IF EXISTS audit_events_refuse_update();
DROP TABLE IF EXISTS audit_events;
