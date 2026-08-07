CREATE TABLE IF NOT EXISTS servers (
	id BIGINT PRIMARY KEY NOT NULL,
	team_id BIGINT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
	name VARCHAR(60) NOT NULL,
	hostname VARCHAR(255) NOT NULL,
	ssh_port INTEGER NOT NULL DEFAULT 22,
	ssh_user VARCHAR(60),
	ssh_private_key BYTEA,
	podman_socket_path VARCHAR(255) NOT NULL,
	is_local BOOLEAN NOT NULL DEFAULT FALSE,
	status VARCHAR(20) NOT NULL DEFAULT 'pending',
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	UNIQUE (team_id, name),
	-- Local servers never hold ssh credentials, remote servers always do.
	CONSTRAINT servers_local_no_ssh CHECK (
		(is_local AND ssh_user IS NULL AND ssh_private_key IS NULL)
		OR
		(NOT is_local AND ssh_user IS NOT NULL AND ssh_private_key IS NOT NULL)
	)
);

CREATE INDEX IF NOT EXISTS idx_servers_team_id ON servers (team_id);
