-- MCP servers are applications that speak MCP: the row says how. `http` servers answer
-- streamable HTTP themselves; `stdio` servers run behind `podkit-agent --mcp-bridge`.
CREATE TABLE mcp_servers (
	application_id BIGINT PRIMARY KEY REFERENCES applications(id) ON DELETE CASCADE,
	transport VARCHAR(8) NOT NULL CHECK (transport IN ('http', 'stdio')),
	-- Endpoint of the streamable HTTP transport inside the container.
	path VARCHAR(200) NOT NULL DEFAULT '/mcp',
	-- stdio only: the command line, one argument per line. NULL runs the image's own
	-- entrypoint and command.
	command TEXT,
	-- The key the web UI uses to list the server's tools; issued with the first request.
	probe_key_hash VARCHAR(64) UNIQUE,
	probe_key BYTEA,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Which MCP servers an application may reach through the gateway. Every link has its own
-- key: only the hash goes to the agent, the key itself is encrypted so each deploy can
-- hand it to the application.
CREATE TABLE application_mcp_links (
	application_id BIGINT NOT NULL REFERENCES applications(id) ON DELETE CASCADE,
	mcp_application_id BIGINT NOT NULL REFERENCES mcp_servers(application_id) ON DELETE RESTRICT,
	key_hash VARCHAR(64) NOT NULL UNIQUE,
	key BYTEA NOT NULL,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	PRIMARY KEY (application_id, mcp_application_id)
);
CREATE INDEX idx_application_mcp_links_server ON application_mcp_links (mcp_application_id);

-- Requests the gateway forwarded, per key, MCP server and hour. Kept after a link or a
-- server is gone, so there are no foreign keys; application_id and team_id are filled in
-- when the key was still known.
CREATE TABLE mcp_usage (
	key_hash VARCHAR(64) NOT NULL,
	mcp_application_id BIGINT NOT NULL,
	hour TIMESTAMPTZ NOT NULL,
	application_id BIGINT,
	team_id BIGINT,
	requests BIGINT NOT NULL DEFAULT 0,
	PRIMARY KEY (key_hash, mcp_application_id, hour)
);
CREATE INDEX idx_mcp_usage_server_hour ON mcp_usage (mcp_application_id, hour);
