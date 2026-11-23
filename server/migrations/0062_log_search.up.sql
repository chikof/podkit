-- Log retention, search and export. The agents keep the container logs on their own disks;
-- this is what a team chose for them: how long, how much and where to forward them.

CREATE TABLE log_settings (
	team_id BIGINT PRIMARY KEY NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
	-- Lines older than this are deleted on every server of the team.
	retention_days INTEGER NOT NULL DEFAULT 7 CHECK (retention_days BETWEEN 1 AND 3650),
	-- Per server: the oldest files go while the logs take more than this.
	max_mb INTEGER NOT NULL DEFAULT 1024 CHECK (max_mb BETWEEN 16 AND 1048576),
	-- Forwarding: NULL for none. 'loki' is the push API, 'otlp' OTLP over HTTP with JSON.
	export_kind VARCHAR(8) CHECK (export_kind IN ('loki', 'otlp')),
	export_url TEXT,
	-- Loki's X-Scope-OrgID.
	export_tenant VARCHAR(200),
	-- The value of the Authorization header, sealed with the server's key. Write-only.
	export_authorization BYTEA,
	updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	CONSTRAINT log_settings_export CHECK ((export_kind IS NULL) = (export_url IS NULL))
);

-- `logs`: read searches and reads the stored logs (owners and members); update changes the
-- retention and the forwarding (owners).
INSERT INTO permissions (id, action, resource)
SELECT (SELECT COALESCE(MAX(id), 0) FROM permissions) + n, action, 'logs'
FROM (VALUES (1, 'read'), (2, 'update')) AS v (n, action)
ON CONFLICT DO NOTHING;
INSERT INTO role_permissions (role_id, permission_id)
	SELECT 1, id FROM permissions WHERE resource = 'logs' ON CONFLICT DO NOTHING;
INSERT INTO role_permissions (role_id, permission_id)
	SELECT 2, id FROM permissions WHERE resource = 'logs' AND action = 'read' ON CONFLICT DO NOTHING;
