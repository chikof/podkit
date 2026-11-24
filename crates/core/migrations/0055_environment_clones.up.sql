-- One clone of an environment: what was asked for, how far it got, and the report.
--
-- The clone's configuration (every resource row) is written in one transaction when the
-- clone is requested, so an environment is either fully copied or not there at all. What
-- runs afterwards (provisioning databases, copying data, deploying) is a job; each step
-- of it is a line of `report`, so a partial failure says what worked and what did not.
CREATE TABLE environment_clones (
	id BIGINT PRIMARY KEY,
	team_id BIGINT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
	project_id BIGINT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
	-- Kept as names, so the record reads right after either environment is deleted.
	source_environment_id BIGINT REFERENCES environments(id) ON DELETE SET NULL,
	source_name VARCHAR(60) NOT NULL,
	environment_id BIGINT REFERENCES environments(id) ON DELETE SET NULL,
	name VARCHAR(60) NOT NULL,
	slug VARCHAR(60) NOT NULL,
	-- The options as asked: secrets mode, data, backup destination, deploy.
	options JSONB NOT NULL,
	-- queued, running, succeeded, partial (some steps failed) or failed.
	status VARCHAR(20) NOT NULL DEFAULT 'queued'
		CHECK (status IN ('queued', 'running', 'succeeded', 'partial', 'failed')),
	-- A list of steps: label, status (pending, running, ok, failed, skipped), detail.
	report JSONB NOT NULL DEFAULT '[]'::jsonb,
	-- Each resource of the source with the resource made from it: kind, source id, new id.
	pairs JSONB NOT NULL DEFAULT '[]'::jsonb,
	-- The job that carries the clone out; its log is the clone's log.
	job_id BIGINT,
	created_by BIGINT REFERENCES users(id) ON DELETE SET NULL,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	finished_at TIMESTAMPTZ
);
CREATE INDEX idx_environment_clones_project ON environment_clones (project_id, id DESC);
