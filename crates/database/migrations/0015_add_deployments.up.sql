CREATE TABLE IF NOT EXISTS deployments (
	id BIGINT PRIMARY KEY NOT NULL,
	application_id BIGINT NOT NULL REFERENCES applications(id) ON DELETE CASCADE,
	status VARCHAR(20) NOT NULL DEFAULT 'queued',
	commit_sha VARCHAR(64),
	image_tag VARCHAR(255),
	container_id VARCHAR(255),
	error_message TEXT,
	triggered_by BIGINT NOT NULL REFERENCES users(id),
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	started_at TIMESTAMPTZ,
	finished_at TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_deployments_application_id ON deployments (application_id);
