CREATE TABLE IF NOT EXISTS custom_domains (
	id BIGINT PRIMARY KEY NOT NULL,
	application_id BIGINT NOT NULL REFERENCES applications(id) ON DELETE CASCADE,
	-- a hostname can only point at one place at a time
	hostname VARCHAR(255) NOT NULL UNIQUE,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_custom_domains_application_id ON custom_domains (application_id);
