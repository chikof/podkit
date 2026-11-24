-- Quotas, usage metering and the billing extension point.
--
-- A limit is a row; a team with no row for a kind has no limit on it, so a homelab that never
-- sets one pays for nothing. `kind` is the key of `quotas::QuotaKind` (applications, cpu_millis,
-- memory_mb, ...), `hard_limit` is in that kind's own unit.
CREATE TABLE team_quotas (
	team_id BIGINT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
	kind VARCHAR(30) NOT NULL,
	hard_limit BIGINT NOT NULL CHECK (hard_limit >= 0),
	updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	PRIMARY KEY (team_id, kind)
);

-- How far each limit has been used, as last announced: 0 below the soft limit (80%), 1 at or
-- over it, 2 at the limit. A notification is sent when the level goes up and never again at the
-- same level, so a team sitting at 85% hears about it once. `period` is `YYYY-MM` for the kinds
-- that reset each month, so a new month starts from 0.
CREATE TABLE quota_levels (
	team_id BIGINT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
	kind VARCHAR(30) NOT NULL,
	period VARCHAR(7) NOT NULL DEFAULT '',
	level SMALLINT NOT NULL DEFAULT 0 CHECK (level BETWEEN 0 AND 2),
	PRIMARY KEY (team_id, kind)
);

-- One row per team and hour in which the team used anything. Written by the hourly metering
-- pass from the samples of the metrics tables and the gateway's and the deployments' own rows;
-- an hour already written is never changed, so what a billing provider was told stays true.
CREATE TABLE usage_records (
	team_id BIGINT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
	hour TIMESTAMPTZ NOT NULL,
	cpu_seconds DOUBLE PRECISION NOT NULL DEFAULT 0,
	memory_gb_hours DOUBLE PRECISION NOT NULL DEFAULT 0,
	disk_gb_hours DOUBLE PRECISION NOT NULL DEFAULT 0,
	gpu_hours DOUBLE PRECISION NOT NULL DEFAULT 0,
	egress_bytes BIGINT NOT NULL DEFAULT 0,
	model_tokens BIGINT NOT NULL DEFAULT 0,
	build_minutes DOUBLE PRECISION NOT NULL DEFAULT 0,
	PRIMARY KEY (team_id, hour)
);
CREATE INDEX idx_usage_records_hour ON usage_records (hour);

-- The id a billing provider gave a team, when one is plugged in (the `cloud` feature). Empty on
-- a self-hosted podkit.
CREATE TABLE billing_customers (
	team_id BIGINT PRIMARY KEY REFERENCES teams(id) ON DELETE CASCADE,
	external_id TEXT NOT NULL,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- `quota`: read shows a team's usage against its limits. Owners (role 1) and members (role 2)
-- read. Changing a limit is not a role's permission: only the instance's administrator may.
INSERT INTO permissions (id, action, resource)
VALUES ((SELECT COALESCE(MAX(id), 0) FROM permissions) + 1, 'read', 'quota')
ON CONFLICT DO NOTHING;
INSERT INTO role_permissions (role_id, permission_id)
	SELECT r.id, p.id FROM (VALUES (1), (2)) AS r (id), permissions p
	WHERE p.resource = 'quota' AND p.action = 'read' ON CONFLICT DO NOTHING;
