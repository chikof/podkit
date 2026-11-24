-- Notifications and alerts: channels, rules, the delivery queue, the in-app bell and metric
-- alerts. Secrets (webhook URLs, bot tokens, signing secrets, the SMTP password) are
-- age-encrypted and never read back.

-- Where a message can go. `config` holds what is safe to show (a Telegram chat id, an ntfy
-- topic, an email address); `secret` is the encrypted JSON of what is not (a webhook URL, a
-- bot token). `hint` is the host of the destination, for the list.
CREATE TABLE notification_channels (
	id BIGINT PRIMARY KEY,
	team_id BIGINT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
	name VARCHAR(60) NOT NULL,
	-- discord, slack, telegram, ntfy, email or webhook.
	kind VARCHAR(20) NOT NULL,
	config JSONB NOT NULL DEFAULT '{}',
	hint VARCHAR(255) NOT NULL DEFAULT '',
	secret BYTEA,
	enabled BOOLEAN NOT NULL DEFAULT TRUE,
	last_test_at TIMESTAMPTZ,
	last_test_ok BOOLEAN,
	last_test_error TEXT,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	CONSTRAINT notification_channels_team_id_name_key UNIQUE (team_id, name)
);

-- The team's SMTP server, shared by its email channels.
CREATE TABLE notification_smtp (
	team_id BIGINT PRIMARY KEY REFERENCES teams(id) ON DELETE CASCADE,
	host VARCHAR(255) NOT NULL,
	port INTEGER NOT NULL,
	-- starttls, tls or none.
	security VARCHAR(10) NOT NULL,
	username VARCHAR(255),
	password BYTEA,
	from_addr VARCHAR(255) NOT NULL
);

-- Noise control. Quiet hours are minutes after midnight in the team's own offset from UTC.
CREATE TABLE notification_settings (
	team_id BIGINT PRIMARY KEY REFERENCES teams(id) ON DELETE CASCADE,
	dedup_minutes INTEGER NOT NULL DEFAULT 10,
	quiet_enabled BOOLEAN NOT NULL DEFAULT FALSE,
	quiet_start_min INTEGER NOT NULL DEFAULT 1320,
	quiet_end_min INTEGER NOT NULL DEFAULT 420,
	utc_offset_min INTEGER NOT NULL DEFAULT 0
);

-- Event kinds to channels. A null project or environment matches any.
CREATE TABLE notification_rules (
	id BIGINT PRIMARY KEY,
	team_id BIGINT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
	name VARCHAR(60) NOT NULL,
	kinds TEXT[] NOT NULL,
	project_id BIGINT REFERENCES projects(id) ON DELETE CASCADE,
	environment_id BIGINT REFERENCES environments(id) ON DELETE CASCADE,
	-- 0 info, 1 warning, 2 critical.
	min_severity SMALLINT NOT NULL DEFAULT 0,
	enabled BOOLEAN NOT NULL DEFAULT TRUE,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	CONSTRAINT notification_rules_team_id_name_key UNIQUE (team_id, name)
);

CREATE TABLE notification_rule_channels (
	rule_id BIGINT NOT NULL REFERENCES notification_rules(id) ON DELETE CASCADE,
	channel_id BIGINT NOT NULL REFERENCES notification_channels(id) ON DELETE CASCADE,
	PRIMARY KEY (rule_id, channel_id)
);

-- Every event the team heard about, which is also the bell's history. Repeats of the same
-- `event_key` inside the dedup window only raise `count`.
CREATE TABLE notification_events (
	id BIGINT PRIMARY KEY,
	team_id BIGINT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
	kind VARCHAR(40) NOT NULL,
	severity SMALLINT NOT NULL,
	event_key VARCHAR(255) NOT NULL,
	title VARCHAR(255) NOT NULL,
	body TEXT NOT NULL DEFAULT '',
	link VARCHAR(500),
	project_id BIGINT,
	environment_id BIGINT,
	count INTEGER NOT NULL DEFAULT 1,
	-- How many occurrences the channels have been told about.
	notified_count INTEGER NOT NULL DEFAULT 1,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	last_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX idx_notification_events_team ON notification_events (team_id, id DESC);
CREATE INDEX idx_notification_events_key ON notification_events (team_id, event_key, last_at DESC);

-- What a person has read. `read_through` marks every event up to an id as read.
CREATE TABLE notification_reads (
	user_id BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
	event_id BIGINT NOT NULL REFERENCES notification_events(id) ON DELETE CASCADE,
	PRIMARY KEY (user_id, event_id)
);
CREATE TABLE notification_user_state (
	user_id BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
	team_id BIGINT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
	read_through BIGINT NOT NULL DEFAULT 0,
	PRIMARY KEY (user_id, team_id)
);

-- The delivery queue. `status` is queued, sending, sent, dead (retries ran out), deferred
-- (waiting for the end of quiet hours) or digested (folded into a digest delivery).
CREATE TABLE notification_deliveries (
	id BIGINT PRIMARY KEY,
	team_id BIGINT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
	channel_id BIGINT NOT NULL REFERENCES notification_channels(id) ON DELETE CASCADE,
	event_id BIGINT REFERENCES notification_events(id) ON DELETE SET NULL,
	message JSONB NOT NULL,
	status VARCHAR(10) NOT NULL DEFAULT 'queued',
	attempts INTEGER NOT NULL DEFAULT 0,
	next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	last_error TEXT,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	sent_at TIMESTAMPTZ
);
CREATE INDEX idx_notification_deliveries_due ON notification_deliveries (status, next_attempt_at);
CREATE INDEX idx_notification_deliveries_team ON notification_deliveries (team_id, created_at DESC);

-- Up/down events that change state too often are muted until they settle.
CREATE TABLE notification_flaps (
	team_id BIGINT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
	flap_key VARCHAR(255) NOT NULL,
	window_start TIMESTAMPTZ NOT NULL,
	last_change TIMESTAMPTZ NOT NULL,
	changes INTEGER NOT NULL,
	flapping BOOLEAN NOT NULL DEFAULT FALSE,
	PRIMARY KEY (team_id, flap_key)
);

-- "Metric X over Y for Z minutes". `scope` is server or resource; a resource is an
-- application, database, model or stack named by `resource_kind` and `resource_id` (no
-- foreign key: metrics rows outlive nothing either).
CREATE TABLE metric_alerts (
	id BIGINT PRIMARY KEY,
	team_id BIGINT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
	name VARCHAR(60) NOT NULL,
	scope VARCHAR(10) NOT NULL,
	server_id BIGINT REFERENCES servers(id) ON DELETE CASCADE,
	resource_kind VARCHAR(20),
	resource_id BIGINT,
	-- cpu, mem, disk, gpu (percent) or mem_mb (resources only).
	metric VARCHAR(10) NOT NULL,
	threshold DOUBLE PRECISION NOT NULL,
	duration_minutes INTEGER NOT NULL,
	severity SMALLINT NOT NULL DEFAULT 1,
	enabled BOOLEAN NOT NULL DEFAULT TRUE,
	-- ok or firing.
	state VARCHAR(10) NOT NULL DEFAULT 'ok',
	breach_since TIMESTAMPTZ,
	fired_at TIMESTAMPTZ,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	CONSTRAINT metric_alerts_team_id_name_key UNIQUE (team_id, name)
);

-- `notification`: read sees the channels, rules, alerts and the delivery log; create, update
-- and delete change them. Anyone in the team has a bell of their own without any of these.
INSERT INTO permissions (id, action, resource)
SELECT (SELECT COALESCE(MAX(id), 0) FROM permissions) + n, action, 'notification'
FROM (VALUES (1, 'create'), (2, 'read'), (3, 'update'), (4, 'delete')) AS v (n, action)
ON CONFLICT DO NOTHING;
INSERT INTO role_permissions (role_id, permission_id)
	SELECT 1, id FROM permissions WHERE resource = 'notification' ON CONFLICT DO NOTHING;
INSERT INTO role_permissions (role_id, permission_id)
	SELECT 2, id FROM permissions WHERE resource = 'notification' AND action = 'read' ON CONFLICT DO NOTHING;
