CREATE TABLE volumes (
	id BIGINT PRIMARY KEY,
	application_id BIGINT NOT NULL REFERENCES applications(id) ON DELETE CASCADE,
	name VARCHAR(100) NOT NULL UNIQUE,
	mount_path VARCHAR(255) NOT NULL,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	UNIQUE (application_id, mount_path)
);
CREATE TABLE backup_targets (
	id BIGINT PRIMARY KEY,
	team_id BIGINT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
	name VARCHAR(60) NOT NULL,
	endpoint TEXT NOT NULL,
	region VARCHAR(60) NOT NULL,
	bucket VARCHAR(255) NOT NULL,
	path_style BOOLEAN NOT NULL DEFAULT FALSE,
	prefix VARCHAR(255) NOT NULL DEFAULT '',
	access_key_id VARCHAR(255) NOT NULL,
	secret_access_key BYTEA NOT NULL,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	UNIQUE (team_id, name)
);
CREATE TABLE databases (
	id BIGINT PRIMARY KEY,
	project_id BIGINT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
	server_id BIGINT NOT NULL REFERENCES servers(id) ON DELETE RESTRICT,
	name VARCHAR(60) NOT NULL,
	engine VARCHAR(20) NOT NULL,
	version VARCHAR(20) NOT NULL,
	credentials BYTEA NOT NULL,
	public_port INTEGER,
	memory_limit_mb INTEGER,
	cpu_limit REAL,
	status VARCHAR(20) NOT NULL DEFAULT 'pending',
	status_message TEXT,
	container_id VARCHAR(255),
	backup_target_id BIGINT REFERENCES backup_targets(id) ON DELETE SET NULL,
	backup_schedule VARCHAR(100),
	backup_keep INTEGER NOT NULL DEFAULT 7,
	backup_next_run_at TIMESTAMPTZ,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	UNIQUE (project_id, name),
	UNIQUE (server_id, public_port)
);
CREATE INDEX idx_databases_server_id ON databases (server_id);
CREATE INDEX idx_databases_backup_due ON databases (backup_next_run_at) WHERE backup_schedule IS NOT NULL;
CREATE TABLE backups (
	id BIGINT PRIMARY KEY,
	database_id BIGINT NOT NULL REFERENCES databases(id) ON DELETE CASCADE,
	target_id BIGINT REFERENCES backup_targets(id) ON DELETE SET NULL,
	status VARCHAR(20) NOT NULL DEFAULT 'queued',
	object_key TEXT,
	size_bytes BIGINT,
	sha256 VARCHAR(64),
	error TEXT,
	triggered_by BIGINT REFERENCES users(id) ON DELETE SET NULL,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	finished_at TIMESTAMPTZ
);
CREATE INDEX idx_backups_database_id_id ON backups (database_id, id DESC);
INSERT INTO permissions (id, action, resource) VALUES
	(37,'create','database'),(38,'read','database'),(39,'update','database'),(40,'delete','database'),
	(41,'create','volume'),(42,'read','volume'),(43,'update','volume'),(44,'delete','volume'),
	(45,'create','backup'),(46,'read','backup'),(47,'update','backup'),(48,'delete','backup')
ON CONFLICT DO NOTHING;
INSERT INTO role_permissions (role_id, permission_id)
	SELECT 1, id FROM permissions WHERE resource IN ('database','volume','backup') ON CONFLICT DO NOTHING;
INSERT INTO role_permissions (role_id, permission_id)
	SELECT 2, id FROM permissions WHERE resource IN ('database','volume','backup') AND action = 'read' ON CONFLICT DO NOTHING;
