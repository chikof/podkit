-- Pull request previews: a temporary copy of an application per open pull request, with
-- fresh empty databases of the same engines (never the production data), its own
-- environment and network, and a link in the pull request.
--
-- An application without a row in `preview_settings` has previews off.
CREATE TABLE preview_settings (
	application_id BIGINT PRIMARY KEY REFERENCES applications(id) ON DELETE CASCADE,
	enabled BOOLEAN NOT NULL DEFAULT FALSE,
	-- Run inside the preview's container once its deploy is healthy: migrate, seed.
	command VARCHAR(500) NOT NULL DEFAULT '',
	-- Open previews at most; a pull request over the limit waits for a free place.
	max_concurrent INTEGER NOT NULL DEFAULT 3 CHECK (max_concurrent BETWEEN 1 AND 20),
	-- A preview with no activity (no push, no `/preview`) for this long is removed.
	ttl_days INTEGER NOT NULL DEFAULT 7 CHECK (ttl_days BETWEEN 1 AND 90),
	-- Caps of the preview's container. NULL: half of the application's own, or 512 MB / 0.5.
	memory_limit_mb INTEGER CHECK (memory_limit_mb IS NULL OR memory_limit_mb >= 64),
	cpu_limit REAL CHECK (cpu_limit IS NULL OR cpu_limit > 0),
	-- Whether a pull request from a fork, once a maintainer approved it, also gets the
	-- variables marked "available to previews". Off: it gets none.
	fork_variables BOOLEAN NOT NULL DEFAULT FALSE,
	updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- One row per pull request that has a preview, or is waiting for one.
--   awaiting_approval  a fork's pull request: a maintainer must comment `/preview`
--   limited            the application's limit of open previews is reached
--   queued, building   the environment exists and the job is bringing it up
--   ready              the preview answers
--   failed             a step failed; the environment stays until the pull request closes
--   removing           the pull request closed (or the preview expired): the job tears down
CREATE TABLE previews (
	id BIGINT PRIMARY KEY,
	team_id BIGINT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
	project_id BIGINT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
	application_id BIGINT NOT NULL REFERENCES applications(id) ON DELETE CASCADE,
	number INTEGER NOT NULL,
	title VARCHAR(200) NOT NULL DEFAULT '',
	head_sha VARCHAR(64) NOT NULL DEFAULT '',
	head_ref VARCHAR(255) NOT NULL DEFAULT '',
	-- `owner/name` of the repository the branch is in; a fork's differs from the application's.
	head_repo VARCHAR(255) NOT NULL DEFAULT '',
	from_fork BOOLEAN NOT NULL DEFAULT FALSE,
	approved BOOLEAN NOT NULL DEFAULT FALSE,
	state VARCHAR(20) NOT NULL CHECK (state IN
		('awaiting_approval', 'limited', 'queued', 'building', 'ready', 'failed', 'removing')),
	detail VARCHAR(500) NOT NULL DEFAULT '',
	environment_id BIGINT REFERENCES environments(id) ON DELETE SET NULL,
	preview_application_id BIGINT REFERENCES applications(id) ON DELETE SET NULL,
	url VARCHAR(300) NOT NULL DEFAULT '',
	-- The provider's id of the comment podkit keeps up to date in the pull request.
	comment_id VARCHAR(40),
	last_activity_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	UNIQUE (application_id, number)
);
CREATE INDEX idx_previews_environment ON previews (environment_id) WHERE environment_id IS NOT NULL;
CREATE INDEX idx_previews_project ON previews (project_id, id DESC);
CREATE INDEX idx_previews_activity ON previews (last_activity_at);

-- Variables are write-only secrets, so a preview gets only those marked as available to it.
ALTER TABLE env_vars ADD COLUMN preview BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE shared_env_vars ADD COLUMN preview BOOLEAN NOT NULL DEFAULT FALSE;
