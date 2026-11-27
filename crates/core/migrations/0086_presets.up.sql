-- A team's presets: a project file (podkit.toml with one project) kept to be applied again,
-- here or in another team. `origin` says where it came from: `saved` from a project of this
-- team, `imported` from a file somebody handed in. No secret is in the file.
CREATE TABLE presets (
	id BIGINT PRIMARY KEY,
	team_id BIGINT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
	slug VARCHAR(60) NOT NULL,
	name VARCHAR(120) NOT NULL,
	description VARCHAR(500) NOT NULL DEFAULT '',
	origin VARCHAR(20) NOT NULL CHECK (origin IN ('saved', 'imported')),
	file TEXT NOT NULL,
	created_by BIGINT REFERENCES users(id) ON DELETE SET NULL,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	UNIQUE (team_id, slug)
);
