-- Uptime checks: probes run from the control plane, their raw results, daily rollups,
-- incidents and the optional public status page.

CREATE TABLE uptime_checks (
	id BIGINT PRIMARY KEY NOT NULL,
	team_id BIGINT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
	-- The application a check was made for; the check goes with it.
	application_id BIGINT REFERENCES applications(id) ON DELETE CASCADE,
	-- `app:<id>:<hostname>` for a check podkit made by itself, so it is not made twice.
	auto_key TEXT,
	name VARCHAR(60) NOT NULL,
	kind VARCHAR(8) NOT NULL CHECK (kind IN ('http', 'tcp')),
	url TEXT,
	method VARCHAR(8) NOT NULL DEFAULT 'GET' CHECK (method IN ('GET', 'HEAD')),
	-- 0 means any answer below 400 (after redirects).
	expected_status INTEGER NOT NULL DEFAULT 200 CHECK (expected_status = 0 OR expected_status BETWEEN 100 AND 599),
	keyword TEXT,
	host TEXT,
	port INTEGER CHECK (port BETWEEN 1 AND 65535),
	timeout_secs INTEGER NOT NULL DEFAULT 10 CHECK (timeout_secs BETWEEN 1 AND 60),
	interval_secs INTEGER NOT NULL DEFAULT 60 CHECK (interval_secs >= 30),
	-- Warn this many days before the certificate expires; NULL for no TLS watch.
	tls_warn_days INTEGER CHECK (tls_warn_days BETWEEN 1 AND 90),
	enabled BOOLEAN NOT NULL DEFAULT TRUE,
	-- Shown on the public status page under `status_name` (or `name`).
	status_visible BOOLEAN NOT NULL DEFAULT FALSE,
	status_name VARCHAR(60),
	-- Current state: 'unknown' until the first result, then 'up' or 'down'.
	state VARCHAR(8) NOT NULL DEFAULT 'unknown' CHECK (state IN ('unknown', 'up', 'down')),
	state_since TIMESTAMPTZ,
	fail_streak INTEGER NOT NULL DEFAULT 0,
	ok_streak INTEGER NOT NULL DEFAULT 0,
	next_check_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	last_checked_at TIMESTAMPTZ,
	last_latency_ms INTEGER,
	last_error TEXT,
	tls_expires_at TIMESTAMPTZ,
	tls_checked_at TIMESTAMPTZ,
	tls_warned BOOLEAN NOT NULL DEFAULT FALSE,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	CONSTRAINT uptime_checks_team_id_name_key UNIQUE (team_id, name),
	CONSTRAINT uptime_checks_target CHECK (
		(kind = 'http' AND url IS NOT NULL) OR (kind = 'tcp' AND host IS NOT NULL AND port IS NOT NULL)
	)
);
CREATE UNIQUE INDEX uptime_checks_auto_key ON uptime_checks (team_id, auto_key) WHERE auto_key IS NOT NULL;
CREATE INDEX uptime_checks_due ON uptime_checks (next_check_at) WHERE enabled;

-- Raw results, kept for 7 days.
CREATE TABLE uptime_results (
	check_id BIGINT NOT NULL REFERENCES uptime_checks(id) ON DELETE CASCADE,
	at TIMESTAMPTZ NOT NULL,
	ok BOOLEAN NOT NULL,
	latency_ms INTEGER,
	status_code INTEGER,
	error TEXT,
	PRIMARY KEY (check_id, at)
);

-- One row per check and UTC day, kept for 90 days.
CREATE TABLE uptime_daily (
	check_id BIGINT NOT NULL REFERENCES uptime_checks(id) ON DELETE CASCADE,
	day DATE NOT NULL,
	total INTEGER NOT NULL,
	ok INTEGER NOT NULL,
	avg_latency_ms INTEGER,
	PRIMARY KEY (check_id, day)
);

CREATE TABLE uptime_incidents (
	id BIGINT PRIMARY KEY NOT NULL,
	check_id BIGINT NOT NULL REFERENCES uptime_checks(id) ON DELETE CASCADE,
	started_at TIMESTAMPTZ NOT NULL,
	ended_at TIMESTAMPTZ,
	error TEXT
);
CREATE INDEX uptime_incidents_check ON uptime_incidents (check_id, started_at DESC);

-- A team's status page lives at /status/<team slug> once it is switched on.
CREATE TABLE uptime_status_pages (
	team_id BIGINT PRIMARY KEY NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
	enabled BOOLEAN NOT NULL DEFAULT FALSE,
	title VARCHAR(60)
);

-- Per team: whether podkit makes checks for application domains by itself, and the ones
-- someone deleted, which must not come back.
CREATE TABLE uptime_settings (
	team_id BIGINT PRIMARY KEY NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
	auto_checks BOOLEAN NOT NULL DEFAULT TRUE
);
CREATE TABLE uptime_dismissed (
	team_id BIGINT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
	auto_key TEXT NOT NULL,
	PRIMARY KEY (team_id, auto_key)
);

-- `uptime`: read sees the checks and their history; create, update and delete change them
-- and the status page. Owners (role 1) get all four, members (role 2) read.
INSERT INTO permissions (id, action, resource)
SELECT (SELECT COALESCE(MAX(id), 0) FROM permissions) + n, action, 'uptime'
FROM (VALUES (1, 'create'), (2, 'read'), (3, 'update'), (4, 'delete')) AS v (n, action)
ON CONFLICT DO NOTHING;
INSERT INTO role_permissions (role_id, permission_id)
	SELECT 1, id FROM permissions WHERE resource = 'uptime' ON CONFLICT DO NOTHING;
INSERT INTO role_permissions (role_id, permission_id)
	SELECT 2, id FROM permissions WHERE resource = 'uptime' AND action = 'read' ON CONFLICT DO NOTHING;
