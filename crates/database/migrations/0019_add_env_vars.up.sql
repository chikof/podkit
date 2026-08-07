CREATE TABLE IF NOT EXISTS env_vars (
	id BIGINT PRIMARY KEY NOT NULL,
	application_id BIGINT NOT NULL REFERENCES applications(id) ON DELETE CASCADE,
	key VARCHAR(255) NOT NULL,
	value BYTEA NOT NULL,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	UNIQUE (application_id, key)
);

CREATE INDEX IF NOT EXISTS idx_env_vars_application_id ON env_vars (application_id);
