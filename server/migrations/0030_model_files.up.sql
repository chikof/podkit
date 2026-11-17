-- One Hugging Face token per team. Write-only: only the age-encrypted blob is stored.
CREATE TABLE huggingface_tokens (
	team_id BIGINT PRIMARY KEY REFERENCES teams(id) ON DELETE CASCADE,
	token BYTEA NOT NULL,
	updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Model weights downloaded into a podman volume on one server.
CREATE TABLE model_files (
	id BIGINT PRIMARY KEY,
	team_id BIGINT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
	server_id BIGINT NOT NULL REFERENCES servers(id) ON DELETE RESTRICT,
	repo VARCHAR(255) NOT NULL,
	revision VARCHAR(255) NOT NULL,
	selection JSONB NOT NULL,
	total_bytes BIGINT NOT NULL,
	downloaded_bytes BIGINT NOT NULL DEFAULT 0,
	status VARCHAR(20) NOT NULL DEFAULT 'queued',
	volume_name VARCHAR(100) NOT NULL UNIQUE,
	error TEXT,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX idx_model_files_team_id ON model_files (team_id, id DESC);
CREATE INDEX idx_model_files_server_id ON model_files (server_id);

INSERT INTO permissions (id, action, resource)
SELECT (SELECT COALESCE(MAX(id), 0) FROM permissions) + n, action, 'model_file'
FROM (VALUES (1, 'create'), (2, 'read'), (3, 'update'), (4, 'delete')) AS v(n, action);

INSERT INTO role_permissions (role_id, permission_id)
	SELECT 1, id FROM permissions WHERE resource = 'model_file' ON CONFLICT DO NOTHING;
INSERT INTO role_permissions (role_id, permission_id)
	SELECT 2, id FROM permissions WHERE resource = 'model_file' AND action = 'read' ON CONFLICT DO NOTHING;
