CREATE TABLE IF NOT EXISTS team_members (
	id BIGINT PRIMARY KEY NOT NULL,
	team_id BIGINT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
	user_id BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
	role_id BIGINT NOT NULL REFERENCES roles(id),
	joined_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	UNIQUE (team_id, user_id)
);

CREATE INDEX IF NOT EXISTS idx_team_members_team_id ON team_members (team_id);
