CREATE TABLE IF NOT EXISTS applications (
	id BIGINT PRIMARY KEY NOT NULL,
	project_id BIGINT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
	server_id BIGINT NOT NULL REFERENCES servers(id) ON DELETE RESTRICT,
	name VARCHAR(60) NOT NULL,
	slug VARCHAR(60) NOT NULL,
	repo_url TEXT NOT NULL,
	git_ref VARCHAR(255) NOT NULL DEFAULT 'main',
	deploy_key BYTEA,
	build_strategy VARCHAR(20) NOT NULL DEFAULT 'dockerfile',
	dockerfile_path VARCHAR(255) NOT NULL DEFAULT 'Dockerfile',
	container_port INTEGER NOT NULL,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	-- Slug is unique per server, not just per project; the generated
	-- subdomain is derived from (slug, server).
	UNIQUE (server_id, slug)
);

CREATE INDEX IF NOT EXISTS idx_applications_project_id ON applications (project_id);
CREATE INDEX IF NOT EXISTS idx_applications_server_id ON applications (server_id);
