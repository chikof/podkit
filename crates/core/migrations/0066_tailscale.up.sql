-- Private access through Tailscale (or a Headscale control server).
--
-- One row per team holds how its sidecars join the tailnet. The auth key is write-only: only
-- the age-encrypted blob is stored, and nothing reads it back out except the job that starts
-- a sidecar.
CREATE TABLE tailscale_settings (
	team_id BIGINT PRIMARY KEY REFERENCES teams(id) ON DELETE CASCADE,
	auth_key BYTEA,
	-- A Headscale (or other control server) URL; NULL joins the Tailscale service.
	login_server TEXT,
	-- Tags the sidecars advertise, each `tag:name`.
	tags TEXT[] NOT NULL DEFAULT '{}',
	updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- An application served on the tailnet by a sidecar container. Absent: no sidecar.
CREATE TABLE tailscale_exposures (
	application_id BIGINT PRIMARY KEY REFERENCES applications(id) ON DELETE CASCADE,
	-- The node name; the address is `<hostname>.<tailnet>.ts.net`.
	hostname TEXT NOT NULL,
	status TEXT NOT NULL DEFAULT 'starting'
		CHECK (status IN ('starting', 'running', 'failed', 'removing')),
	-- The node's full DNS name once it has joined, without the trailing dot.
	dns_name TEXT,
	error TEXT,
	-- The sidecar's container, kept so `tailscale logout` can run in it before it is removed.
	container_id TEXT,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
