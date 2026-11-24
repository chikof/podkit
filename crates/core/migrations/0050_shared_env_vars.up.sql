-- Shared environment variables (roadmap A4): set once at team, project or environment
-- scope and inherited by the applications and stacks below. An application's own
-- variable of the same name wins (team < project < environment < app).
--
-- Scope is by which columns are set: a team variable has neither `project_id` nor
-- `environment_id`; a project variable has `project_id`; an environment variable has both
-- (its project is implied, kept so a lookup needs no join).
CREATE TABLE shared_env_vars (
	id BIGINT PRIMARY KEY NOT NULL,
	team_id BIGINT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
	project_id BIGINT REFERENCES projects(id) ON DELETE CASCADE,
	environment_id BIGINT REFERENCES environments(id) ON DELETE CASCADE,
	key VARCHAR(255) NOT NULL,
	-- age-encrypted with the instance secret box; never stored or returned as plaintext.
	value BYTEA NOT NULL,
	-- A secret's value is never shown again; a plain variable's value is shown on the page of
	-- its own scope (never on a page that merely inherits it).
	is_secret BOOLEAN NOT NULL DEFAULT TRUE,
	created_by BIGINT REFERENCES users(id) ON DELETE SET NULL,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	CHECK (environment_id IS NULL OR project_id IS NOT NULL)
);

CREATE UNIQUE INDEX shared_env_vars_team_key
	ON shared_env_vars (team_id, key) WHERE project_id IS NULL;
CREATE UNIQUE INDEX shared_env_vars_project_key
	ON shared_env_vars (project_id, key) WHERE project_id IS NOT NULL AND environment_id IS NULL;
CREATE UNIQUE INDEX shared_env_vars_environment_key
	ON shared_env_vars (environment_id, key) WHERE environment_id IS NOT NULL;
