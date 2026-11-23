-- Server firewall (opt-in, nftables, owned by the podkit-firewall helper on the server).
-- This is what the team wants the firewall to be; the helper holds what is in force. A
-- server without a row has the firewall off.
CREATE TABLE server_firewalls (
	server_id BIGINT PRIMARY KEY REFERENCES servers(id) ON DELETE CASCADE,
	-- Whether the team wants the ruleset on. Set by Apply, cleared by Turn off.
	enabled BOOLEAN NOT NULL DEFAULT FALSE,
	-- SSH is limited to these ranges when the list is not empty. Empty is anywhere.
	-- Open SSH sessions (podkit's own among them) are always kept allowed by the helper.
	ssh_sources TEXT[] NOT NULL DEFAULT '{}',
	-- The team asked for the helper (the root part) to be installed by the next bootstrap.
	install_helper BOOLEAN NOT NULL DEFAULT FALSE,
	updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Explicit allows beyond the defaults (established, loopback, ping, SSH, 80, 443).
CREATE TABLE server_firewall_rules (
	id BIGINT PRIMARY KEY,
	server_id BIGINT NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
	proto TEXT NOT NULL CHECK (proto IN ('tcp', 'udp')),
	port INTEGER NOT NULL CHECK (port BETWEEN 1 AND 65535),
	-- A CIDR range or NULL for anyone.
	source TEXT,
	note VARCHAR(80) NOT NULL DEFAULT '',
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	UNIQUE NULLS NOT DISTINCT (server_id, proto, port, source)
);
CREATE INDEX server_firewall_rules_server_idx ON server_firewall_rules (server_id);
