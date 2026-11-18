-- A model: one inference engine container serving the files of one model_files row.
CREATE TABLE models (
	id BIGINT PRIMARY KEY,
	project_id BIGINT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
	server_id BIGINT NOT NULL REFERENCES servers(id) ON DELETE RESTRICT,
	model_files_id BIGINT NOT NULL REFERENCES model_files(id) ON DELETE RESTRICT,
	name VARCHAR(60) NOT NULL,
	-- The internal DNS name is model-<slug> on every server, so it is unique across teams.
	slug VARCHAR(60) NOT NULL UNIQUE,
	engine VARCHAR(30) NOT NULL,
	-- Resolved when the model is created, so a template update never changes a model that exists.
	image VARCHAR(255) NOT NULL,
	port INTEGER NOT NULL,
	-- File or directory inside the model files volume; empty for the volume root.
	model_path VARCHAR(500) NOT NULL DEFAULT '',
	served_name VARCHAR(100) NOT NULL,
	context_size INTEGER NOT NULL,
	gpu BOOLEAN NOT NULL DEFAULT FALSE,
	extra_args JSONB NOT NULL DEFAULT '[]',
	memory_limit_mb INTEGER,
	status VARCHAR(20) NOT NULL DEFAULT 'starting',
	error TEXT,
	container_id VARCHAR(255),
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	UNIQUE (project_id, name)
);
CREATE INDEX idx_models_server_id ON models (server_id);
CREATE INDEX idx_models_model_files_id ON models (model_files_id);

INSERT INTO permissions (id, action, resource)
SELECT (SELECT COALESCE(MAX(id), 0) FROM permissions) + n, action, 'model'
FROM (VALUES (1, 'create'), (2, 'read'), (3, 'update'), (4, 'delete')) AS v(n, action);

INSERT INTO role_permissions (role_id, permission_id)
	SELECT 1, id FROM permissions WHERE resource = 'model' ON CONFLICT DO NOTHING;
INSERT INTO role_permissions (role_id, permission_id)
	SELECT 2, id FROM permissions WHERE resource = 'model' AND action = 'read' ON CONFLICT DO NOTHING;
