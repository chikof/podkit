-- Saved registry logins: one per team, picked by image apps and stacks
-- instead of pasting credentials again.
CREATE TABLE registries (
	id BIGINT PRIMARY KEY,
	team_id BIGINT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
	name VARCHAR(60) NOT NULL,
	-- Registry host[:port], for example `ghcr.io`; never a scheme or a path.
	url VARCHAR(255) NOT NULL,
	username VARCHAR(255) NOT NULL,
	-- age-encrypted; write-only.
	password BYTEA NOT NULL,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	UNIQUE (team_id, name),
	UNIQUE (team_id, url, username)
);

-- A registry in use cannot be deleted: the next deploy would pull anonymously.
ALTER TABLE applications ADD COLUMN registry_id BIGINT REFERENCES registries(id) ON DELETE RESTRICT;
ALTER TABLE stacks ADD COLUMN registry_id BIGINT REFERENCES registries(id) ON DELETE RESTRICT;
CREATE INDEX idx_applications_registry_id ON applications (registry_id) WHERE registry_id IS NOT NULL;
CREATE INDEX idx_stacks_registry_id ON stacks (registry_id) WHERE registry_id IS NOT NULL;

INSERT INTO permissions (id, action, resource)
SELECT (SELECT COALESCE(MAX(id), 0) FROM permissions) + n, action, 'registry'
FROM (VALUES (1, 'create'), (2, 'read'), (3, 'update'), (4, 'delete')) AS v (n, action)
ON CONFLICT DO NOTHING;
INSERT INTO role_permissions (role_id, permission_id)
	SELECT 1, id FROM permissions WHERE resource = 'registry' ON CONFLICT DO NOTHING;
INSERT INTO role_permissions (role_id, permission_id)
	SELECT 2, id FROM permissions WHERE resource = 'registry' AND action = 'read' ON CONFLICT DO NOTHING;

-- Scoped API tokens. Only the SHA-256 of the secret is stored; the token is
-- shown once, at creation, as `pk_<prefix>_<secret>`.
CREATE TABLE api_tokens (
	id BIGINT PRIMARY KEY,
	team_id BIGINT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
	user_id BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
	name VARCHAR(60) NOT NULL,
	prefix VARCHAR(16) NOT NULL UNIQUE,
	hash BYTEA NOT NULL,
	scopes TEXT[] NOT NULL,
	-- Empty means every project of the team.
	project_ids BIGINT[] NOT NULL DEFAULT '{}',
	expires_at TIMESTAMPTZ,
	last_used_at TIMESTAMPTZ,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	UNIQUE (user_id, team_id, name)
);
CREATE INDEX idx_api_tokens_team_user ON api_tokens (team_id, user_id);
