-- Scheduled tasks (A6): a command run on a cron schedule, either inside a running application's
-- container (`exec`) or in a one-shot container that is removed afterwards (`container`).

CREATE TABLE cron_tasks (
	id BIGINT PRIMARY KEY,
	project_id BIGINT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
	environment_id BIGINT NOT NULL REFERENCES environments(id) ON DELETE CASCADE,
	server_id BIGINT NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
	-- `exec` needs one. A `container` task takes its env, networks and (without `image`) its
	-- image from it.
	application_id BIGINT REFERENCES applications(id) ON DELETE CASCADE,
	name VARCHAR(100) NOT NULL,
	kind VARCHAR(16) NOT NULL,
	-- A `container` task without an application runs this image.
	image TEXT,
	-- Run by `sh -c`.
	command TEXT NOT NULL,
	-- Five fields, read in `timezone`.
	schedule VARCHAR(100) NOT NULL,
	timezone VARCHAR(64) NOT NULL DEFAULT 'UTC',
	timeout_s INTEGER NOT NULL DEFAULT 300,
	concurrency VARCHAR(8) NOT NULL DEFAULT 'skip',
	max_retries INTEGER NOT NULL DEFAULT 0,
	retry_backoff_s INTEGER NOT NULL DEFAULT 60,
	enabled BOOLEAN NOT NULL DEFAULT TRUE,
	-- The next scheduled run; NULL while paused or when the schedule cannot be evaluated.
	next_run_at TIMESTAMPTZ,
	-- How the last finished run ended (`succeeded` or `failed`), for the recovery event.
	last_outcome VARCHAR(16),
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	CONSTRAINT cron_tasks_kind_check CHECK (kind IN ('exec', 'container')),
	CONSTRAINT cron_tasks_concurrency_check CHECK (concurrency IN ('skip', 'queue', 'allow')),
	CONSTRAINT cron_tasks_target_check CHECK (
		(kind = 'exec' AND application_id IS NOT NULL)
		OR (kind = 'container' AND (application_id IS NOT NULL OR image IS NOT NULL))
	),
	CONSTRAINT cron_tasks_environment_id_name_key UNIQUE (environment_id, name)
);

CREATE INDEX idx_cron_tasks_due ON cron_tasks (next_run_at) WHERE enabled;
CREATE INDEX idx_cron_tasks_application ON cron_tasks (application_id);

CREATE TABLE cron_runs (
	id BIGINT PRIMARY KEY,
	task_id BIGINT NOT NULL REFERENCES cron_tasks(id) ON DELETE CASCADE,
	-- The job that runs it and keeps its log; gone once jobs are purged.
	job_id BIGINT REFERENCES jobs(id) ON DELETE SET NULL,
	status VARCHAR(16) NOT NULL DEFAULT 'queued',
	-- `schedule`, `catch_up`, `manual` or `retry`.
	trigger VARCHAR(16) NOT NULL,
	attempt INTEGER NOT NULL DEFAULT 1,
	-- The slot of the schedule it stands for; NULL for a manual run.
	scheduled_for TIMESTAMPTZ,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	started_at TIMESTAMPTZ,
	finished_at TIMESTAMPTZ,
	exit_code INTEGER,
	error TEXT,
	CONSTRAINT cron_runs_status_check CHECK (
		status IN ('queued', 'running', 'succeeded', 'failed', 'cancelled', 'skipped')
	),
	CONSTRAINT cron_runs_trigger_check CHECK (trigger IN ('schedule', 'catch_up', 'manual', 'retry'))
);

CREATE INDEX idx_cron_runs_task ON cron_runs (task_id, id DESC);
CREATE INDEX idx_cron_runs_active ON cron_runs (task_id) WHERE status IN ('queued', 'running');

-- `cron`: read sees tasks and their runs; create, update and delete change them. Update also
-- covers run now, cancel, pause and resume.
INSERT INTO permissions (id, action, resource)
SELECT (SELECT COALESCE(MAX(id), 0) FROM permissions) + n, action, 'cron'
FROM (VALUES (1, 'create'), (2, 'read'), (3, 'update'), (4, 'delete')) AS v (n, action)
ON CONFLICT DO NOTHING;
INSERT INTO role_permissions (role_id, permission_id)
	SELECT 1, id FROM permissions WHERE resource = 'cron' ON CONFLICT DO NOTHING;
INSERT INTO role_permissions (role_id, permission_id)
	SELECT 2, id FROM permissions WHERE resource = 'cron' AND action = 'read' ON CONFLICT DO NOTHING;
