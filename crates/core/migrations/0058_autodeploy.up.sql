-- Push-to-deploy that is safe to leave on. An application without a row in
-- `autodeploy_settings` keeps the plain behaviour: a push to the branch it follows deploys at
-- once, nothing is posted back, and nothing waits.
CREATE TABLE autodeploy_settings (
	application_id BIGINT PRIMARY KEY REFERENCES applications(id) ON DELETE CASCADE,
	-- Off: pushes are recorded and ignored.
	enabled BOOLEAN NOT NULL DEFAULT TRUE,
	-- Globs of the refs whose push deploys. Empty means the ref the application follows.
	branch_patterns TEXT[] NOT NULL DEFAULT '{}',
	-- Whether a tag push is matched against the same patterns.
	deploy_tags BOOLEAN NOT NULL DEFAULT FALSE,
	-- Include and exclude (`!`-prefixed) globs against the files a push changed. Empty means
	-- every push deploys.
	path_filters TEXT[] NOT NULL DEFAULT '{}',
	-- Wait for the commit's checks (check suites and commit statuses, pipelines) to pass.
	wait_for_ci BOOLEAN NOT NULL DEFAULT FALSE,
	ci_timeout_secs INTEGER NOT NULL DEFAULT 900 CHECK (ci_timeout_secs BETWEEN 30 AND 86400),
	-- When the wait runs out: deploy anyway (true) or give up and say so (false).
	deploy_on_ci_timeout BOOLEAN NOT NULL DEFAULT FALSE,
	-- Post pending, success and failure back to the provider as a commit status.
	post_status BOOLEAN NOT NULL DEFAULT TRUE,
	updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Times when pushes are held instead of deployed. `recurring` repeats every week between two
-- minutes of the week (Monday 00:00 is 0) in a fixed UTC offset; the end may be before the
-- start, which wraps over the weekend. `once` is one span of time.
CREATE TABLE autodeploy_freezes (
	id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
	application_id BIGINT NOT NULL REFERENCES applications(id) ON DELETE CASCADE,
	kind VARCHAR(9) NOT NULL CHECK (kind IN ('recurring', 'once')),
	start_minute INTEGER CHECK (start_minute BETWEEN 0 AND 10079),
	end_minute INTEGER CHECK (end_minute BETWEEN 0 AND 10079),
	utc_offset_minutes INTEGER NOT NULL DEFAULT 0 CHECK (utc_offset_minutes BETWEEN -840 AND 840),
	starts_at TIMESTAMPTZ,
	ends_at TIMESTAMPTZ,
	note VARCHAR(120) NOT NULL DEFAULT '',
	created_by BIGINT REFERENCES users(id) ON DELETE SET NULL,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	CONSTRAINT autodeploy_freezes_shape CHECK (
		(kind = 'recurring' AND start_minute IS NOT NULL AND end_minute IS NOT NULL
			AND start_minute <> end_minute)
		OR (kind = 'once' AND starts_at IS NOT NULL AND ends_at IS NOT NULL AND ends_at > starts_at)
	)
);
CREATE INDEX autodeploy_freezes_application ON autodeploy_freezes (application_id);

-- One row per push that reached the rules. `waiting_ci` and `frozen` are the queue the
-- background pass works through; every other state is a verdict.
--   waiting_ci  the commit's checks have not finished
--   frozen      checks passed (or none were asked for) but a freeze window is open
--   deployed    a deployment was queued (`deployment_id`)
--   superseded  a newer push replaced it before it deployed, or while its deployment was
--               still waiting to start
--   skipped     the commit message asked for it ([skip deploy], [skip ci])
--   filtered    no changed file passed the path filters
--   ci_failed   the checks failed; nothing was deployed
--   ci_timeout  the checks did not finish in time; nothing was deployed
CREATE TABLE autodeploy_pushes (
	id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
	application_id BIGINT NOT NULL REFERENCES applications(id) ON DELETE CASCADE,
	git_ref VARCHAR(255) NOT NULL,
	commit_sha VARCHAR(64) NOT NULL DEFAULT '',
	message VARCHAR(200) NOT NULL DEFAULT '',
	state VARCHAR(12) NOT NULL CHECK (state IN (
		'waiting_ci', 'frozen', 'deployed', 'superseded', 'skipped', 'filtered',
		'ci_failed', 'ci_timeout'
	)),
	detail VARCHAR(500) NOT NULL DEFAULT '',
	deployment_id BIGINT REFERENCES deployments(id) ON DELETE SET NULL,
	-- When the wait for checks gives up.
	ci_deadline TIMESTAMPTZ,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX autodeploy_pushes_application ON autodeploy_pushes (application_id, id DESC);
CREATE INDEX autodeploy_pushes_pending ON autodeploy_pushes (state) WHERE state IN ('waiting_ci', 'frozen');
CREATE INDEX autodeploy_pushes_deployment ON autodeploy_pushes (deployment_id) WHERE deployment_id IS NOT NULL;
