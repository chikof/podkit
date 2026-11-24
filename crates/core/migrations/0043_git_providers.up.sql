-- Git providers: a team connects a GitHub App, a GitLab or a Gitea once, and applications pick a
-- repository from it instead of carrying their own token. The credentials (a token, or a GitHub
-- App's private key and webhook secret) are one age-encrypted JSON document, written once and
-- never read back out by the API or the UI.
CREATE TABLE git_providers (
	id BIGINT PRIMARY KEY,
	team_id BIGINT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
	kind VARCHAR(20) NOT NULL CHECK (kind IN ('github_app', 'gitlab', 'gitea')),
	name VARCHAR(120) NOT NULL,
	-- The web address of the instance: https://github.com, https://gitlab.example.com, ...
	base_url TEXT NOT NULL,
	credentials BYTEA NOT NULL,
	created_by BIGINT REFERENCES users(id) ON DELETE SET NULL,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	UNIQUE (id, team_id),
	UNIQUE (team_id, name)
);
CREATE INDEX idx_git_providers_team_id ON git_providers (team_id);

-- An application cloned through a provider names it and the repository ("owner/name").
-- `git_hook_id` is the id of the push webhook podkit made for it at the provider, if it did.
ALTER TABLE applications
	ADD COLUMN git_provider_id BIGINT REFERENCES git_providers(id),
	ADD COLUMN git_repo TEXT,
	ADD COLUMN git_hook_id TEXT,
	ADD CONSTRAINT applications_git_provider_repo
		CHECK ((git_provider_id IS NULL) = (git_repo IS NULL));
CREATE INDEX idx_applications_git_provider_id ON applications (git_provider_id);

-- Deliveries of provider webhooks already acted on, so a captured request cannot be replayed.
CREATE TABLE git_webhook_deliveries (
	delivery_id VARCHAR(200) PRIMARY KEY,
	seen_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX idx_git_webhook_deliveries_seen_at ON git_webhook_deliveries (seen_at);

INSERT INTO permissions (id, action, resource)
SELECT (SELECT COALESCE(MAX(id), 0) FROM permissions) + n, action, 'git_provider'
FROM (VALUES (1, 'create'), (2, 'read'), (3, 'update'), (4, 'delete')) AS v (n, action)
ON CONFLICT DO NOTHING;
INSERT INTO role_permissions (role_id, permission_id)
	SELECT 1, id FROM permissions WHERE resource = 'git_provider' ON CONFLICT DO NOTHING;
INSERT INTO role_permissions (role_id, permission_id)
	SELECT 2, id FROM permissions WHERE resource = 'git_provider' AND action = 'read' ON CONFLICT DO NOTHING;
