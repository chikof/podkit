ALTER TABLE servers ADD COLUMN host_key TEXT;
ALTER TABLE servers ADD COLUMN agent_socket_path VARCHAR(255);
ALTER TABLE servers ADD COLUMN agent_version VARCHAR(40);
ALTER TABLE servers ADD COLUMN arch VARCHAR(20);
ALTER TABLE servers ADD COLUMN status_message TEXT;

-- Remote servers must be bootstrapped with the agent before they count as ready.
UPDATE servers SET status = 'pending' WHERE NOT is_local;

CREATE TABLE jobs (
	id BIGINT PRIMARY KEY,
	kind VARCHAR(30) NOT NULL,
	server_id BIGINT NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
	target_id BIGINT NOT NULL,
	lock_key VARCHAR(60) NOT NULL,
	status VARCHAR(20) NOT NULL DEFAULT 'queued',
	error TEXT,
	run_after TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	started_at TIMESTAMPTZ,
	finished_at TIMESTAMPTZ
);

CREATE INDEX idx_jobs_active ON jobs (status, run_after, id) WHERE status IN ('queued', 'running');
CREATE INDEX idx_jobs_kind_target ON jobs (kind, target_id);
