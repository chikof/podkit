-- Moving an application to another server of its team, and draining a server: moving
-- everything that can move off it, one application at a time.

-- A draining server takes no new resources: the create forms leave it out and the API
-- refuses it. It stays draining until someone stops it, also after the drain finished.
ALTER TABLE servers ADD COLUMN draining_since TIMESTAMPTZ;

-- One drain of one server: what it found and what became of each item, in `report`.
CREATE TABLE server_drains (
	id BIGINT PRIMARY KEY,
	team_id BIGINT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
	server_id BIGINT NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
	-- Where the applications go; the name is kept for the history once the server is gone.
	target_server_id BIGINT REFERENCES servers(id) ON DELETE SET NULL,
	target_server_name VARCHAR(60) NOT NULL,
	-- Volume data is copied through this destination.
	backup_target_id BIGINT REFERENCES backup_targets(id) ON DELETE SET NULL,
	status VARCHAR(16) NOT NULL DEFAULT 'queued'
		CHECK (status IN ('queued', 'running', 'succeeded', 'partial', 'failed', 'cancelled')),
	-- One entry per thing found on the server: kind, id, name, outcome, detail.
	report JSONB NOT NULL DEFAULT '[]',
	requested_by BIGINT REFERENCES users(id) ON DELETE SET NULL,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	started_at TIMESTAMPTZ,
	finished_at TIMESTAMPTZ
);
CREATE INDEX idx_server_drains_server ON server_drains (server_id, id DESC);
CREATE UNIQUE INDEX server_drains_one_active ON server_drains (server_id)
	WHERE status IN ('queued', 'running');

-- One move of one application. `switched_at` is set once `applications.server_id` points
-- at the target, so a move that is interrupted after it can be put back.
CREATE TABLE application_moves (
	id BIGINT PRIMARY KEY,
	application_id BIGINT NOT NULL REFERENCES applications(id) ON DELETE CASCADE,
	from_server_id BIGINT REFERENCES servers(id) ON DELETE SET NULL,
	to_server_id BIGINT REFERENCES servers(id) ON DELETE SET NULL,
	from_server_name VARCHAR(60) NOT NULL,
	to_server_name VARCHAR(60) NOT NULL,
	-- `redeploy`: the running image is copied and started there before the old container
	-- goes; `stop_copy`: stopped, volumes copied, started there; `rebuild`: the old server is
	-- unreachable, so it is built or pulled again there.
	method VARCHAR(16) NOT NULL CHECK (method IN ('redeploy', 'stop_copy', 'rebuild')),
	status VARCHAR(16) NOT NULL DEFAULT 'queued'
		CHECK (status IN ('queued', 'running', 'succeeded', 'failed', 'cancelled')),
	-- What it is doing now, or how it ended.
	step VARCHAR(300) NOT NULL DEFAULT '',
	error TEXT,
	deployment_id BIGINT REFERENCES deployments(id) ON DELETE SET NULL,
	backup_target_id BIGINT REFERENCES backup_targets(id) ON DELETE SET NULL,
	drain_id BIGINT REFERENCES server_drains(id) ON DELETE SET NULL,
	requested_by BIGINT REFERENCES users(id) ON DELETE SET NULL,
	switched_at TIMESTAMPTZ,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	started_at TIMESTAMPTZ,
	finished_at TIMESTAMPTZ
);
CREATE INDEX idx_application_moves_application ON application_moves (application_id, id DESC);
CREATE INDEX idx_application_moves_drain ON application_moves (drain_id) WHERE drain_id IS NOT NULL;
CREATE UNIQUE INDEX application_moves_one_active ON application_moves (application_id)
	WHERE status IN ('queued', 'running');
