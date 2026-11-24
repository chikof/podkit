DELETE FROM role_permissions WHERE permission_id IN (SELECT id FROM permissions WHERE resource = 'uptime');
DELETE FROM permissions WHERE resource = 'uptime';
DROP TABLE uptime_dismissed;
DROP TABLE uptime_settings;
DROP TABLE uptime_status_pages;
DROP TABLE uptime_incidents;
DROP TABLE uptime_daily;
DROP TABLE uptime_results;
DROP TABLE uptime_checks;
