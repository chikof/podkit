-- The guided infrastructure setup keeps only two things per team: whether it was
-- dismissed, and which optional parts the team chose to skip. Whether a step is done
-- is never stored: it is derived from the servers, keys, networks and projects the
-- team really has (server/src/infrastructure/setup.rs).
CREATE TABLE infrastructure_setup (
	team_id BIGINT PRIMARY KEY REFERENCES teams(id) ON DELETE CASCADE,
	dismissed_at TIMESTAMPTZ,
	-- `networks`, `ssh_key`, `git_provider`, `registry`.
	skipped TEXT[] NOT NULL DEFAULT '{}',
	updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
